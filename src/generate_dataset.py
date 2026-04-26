#!/usr/bin/env python3
#
# Copyright Thomas Kessler <tom@kessler.group> All Rights Reserved.
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# generate_dataset.py
#
# Reads images from an AWS S3 bucket (same bucket as the Rust rustslinger program),
# runs Apple Vision QR detection on each image (mac2.metal / Apple Neural Engine),
# extracts telecom equipment labels from S3 metadata and filename,
# and writes COCO-format JSON training data for both QR codes and telecom equipment.
#
# Requirements:
#   pip install boto3 pyobjc-framework-Vision pyobjc-framework-Quartz
#
# Run on macOS only (AWS mac2.metal or local Apple Silicon/Intel Mac).

from __future__ import annotations

import argparse
import hashlib
import json
import os
import queue
import re
import sys
import tempfile
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

import boto3

# Apple frameworks — macOS only
try:
    import Quartz
    import Vision
    from Foundation import NSData
except ImportError:
    sys.exit(
        "ERROR: PyObjC Vision/Quartz frameworks not found.\n"
        "Install with: pip install pyobjc-framework-Vision pyobjc-framework-Quartz"
    )

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

DOWNLOAD_WORKERS = 50     # I/O bound — generous concurrency
VISION_WORKERS   = 16     # Neural Engine bound — matches mac2.metal core count
QUEUE_MAXSIZE    = 200    # ~500 MB in-flight cap (200 × ~2.5 MB)

CHECKPOINT_EVERY = 5_000  # write partial COCO JSON every N images

# COCO category IDs
CAT_QR         = 1
CAT_TOWER      = 2
CAT_GENSET     = 3
CAT_BATTERY    = 4
CAT_SHELTER    = 5
CAT_RECTIFIER  = 6

CATEGORIES = [
    {"id": CAT_QR,        "name": "qr_code",    "supercategory": "barcode"},
    {"id": CAT_TOWER,     "name": "tower",       "supercategory": "telecom"},
    {"id": CAT_GENSET,    "name": "genset",      "supercategory": "telecom"},
    {"id": CAT_BATTERY,   "name": "battery",     "supercategory": "telecom"},
    {"id": CAT_SHELTER,   "name": "shelter",     "supercategory": "telecom"},
    {"id": CAT_RECTIFIER, "name": "rectifier",   "supercategory": "telecom"},
]

# Keyword → category_id for telecom label extraction
LABEL_MAP: dict[str, int] = {
    "tower":      CAT_TOWER,
    "mast":       CAT_TOWER,
    "monopole":   CAT_TOWER,
    "antenna":    CAT_TOWER,
    "pylon":      CAT_TOWER,
    "genset":     CAT_GENSET,
    "generator":  CAT_GENSET,
    "dg":         CAT_GENSET,
    "diesel":     CAT_GENSET,
    "battery":    CAT_BATTERY,
    "batteries":  CAT_BATTERY,
    "batt":       CAT_BATTERY,
    "ups":        CAT_BATTERY,
    "shelter":    CAT_SHELTER,
    "cabin":      CAT_SHELTER,
    "cabinet":    CAT_SHELTER,
    "bts":        CAT_SHELTER,
    "idu":        CAT_SHELTER,
    "rectifier":  CAT_RECTIFIER,
    "rect":       CAT_RECTIFIER,
    "psu":        CAT_RECTIFIER,
    "charger":    CAT_RECTIFIER,
}

# Sentinel for graceful worker shutdown
SENTINEL = object()

# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------

@dataclass
class BBox:
    x: float   # absolute pixels, COCO format (top-left corner)
    y: float
    w: float
    h: float

@dataclass
class Corners:
    """Four corners of QR code in reading order: TL, TR, BR, BL."""
    tl_x: float; tl_y: float
    tr_x: float; tr_y: float
    br_x: float; br_y: float
    bl_x: float; bl_y: float

@dataclass
class QrAnnotation:
    bbox:    BBox
    corners: Optional[Corners]

@dataclass
class TelecomLabel:
    category_id: int
    confidence:  str   # "high" | "medium" | "conflict" | "unknown"

@dataclass
class ImageRecord:
    s3_key:    str
    data:      bytes
    sha256:    str
    width:     int
    height:    int
    qr_anns:  list[QrAnnotation]
    tel_labels: list[TelecomLabel]

# ---------------------------------------------------------------------------
# Coordinate helpers
# ---------------------------------------------------------------------------

def vision_to_coco(
    vx: float, vy: float, vw: float, vh: float,
    img_w: int, img_h: int,
) -> BBox:
    """Convert Vision normalised bbox → COCO absolute pixels.

    Vision (CoreGraphics) has Y=0 at bottom-left.
    COCO has Y=0 at top-left.  The flip is the critical line below.
    """
    x = vx * img_w
    y = (1.0 - vy - vh) * img_h   # Y-axis flip
    w = vw * img_w
    h = vh * img_h
    return BBox(round(x, 2), round(y, 2), round(w, 2), round(h, 2))


def vision_point_to_coco(px: float, py: float, img_w: int, img_h: int) -> tuple[float, float]:
    """Convert a single Vision normalised point → COCO absolute pixels."""
    return round(px * img_w, 2), round((1.0 - py) * img_h, 2)


def assign_split(sha256: str) -> str:
    """Deterministic, reproducible train/val/test split derived from image hash."""
    v = int(sha256[-4:], 16) % 100
    if v < 80:
        return "train"
    if v < 90:
        return "val"
    return "test"

# ---------------------------------------------------------------------------
# Telecom label extraction
# ---------------------------------------------------------------------------

_TOKEN_RE = re.compile(r"[a-z0-9]+")


def _tokens_from_string(s: str) -> set[str]:
    return set(_TOKEN_RE.findall(s.lower()))


def labels_from_key(s3_key: str) -> set[int]:
    """Extract category IDs from S3 key path components."""
    tokens = _tokens_from_string(s3_key)
    return {LABEL_MAP[t] for t in tokens if t in LABEL_MAP}


def labels_from_metadata(meta: dict) -> set[int]:
    """Extract category IDs from S3 object metadata (all values concatenated)."""
    combined = " ".join(str(v) for v in meta.values())
    tokens = _tokens_from_string(combined)
    return {LABEL_MAP[t] for t in tokens if t in LABEL_MAP}


def resolve_labels(s3_key: str, meta: dict) -> list[TelecomLabel]:
    """Merge labels from key and metadata, annotating each with a confidence tier."""
    from_key  = labels_from_key(s3_key)
    from_meta = labels_from_metadata(meta)
    all_ids   = from_key | from_meta

    results: list[TelecomLabel] = []
    for cat_id in all_ids:
        in_key  = cat_id in from_key
        in_meta = cat_id in from_meta
        if in_key and in_meta:
            confidence = "high"
        elif in_key or in_meta:
            confidence = "medium"
        else:
            confidence = "conflict"
        results.append(TelecomLabel(cat_id, confidence))

    return results

# ---------------------------------------------------------------------------
# Apple Vision QR detection
# ---------------------------------------------------------------------------

def detect_qr(image_data: bytes, img_w: int, img_h: int) -> list[QrAnnotation]:
    """Run Vision barcode detection on raw image bytes.

    Thread-safe: uses CGImageSource (CoreGraphics), not NSImage.
    payloadStringValue is deliberately NOT captured — we never use competitor
    decoded content as training labels.
    """
    ns_data = NSData.dataWithBytes_length_(image_data, len(image_data))
    source  = Quartz.CGImageSourceCreateWithData(ns_data, None)
    if source is None:
        return []

    cg = Quartz.CGImageSourceCreateImageAtIndex(source, 0, None)
    if cg is None:
        return []

    req = Vision.VNDetectBarcodesRequest.alloc().init()
    req.setSymbologies_([Vision.VNBarcodeSymbologyQR])

    handler = Vision.VNImageRequestHandler.alloc().initWithCGImage_options_(cg, {})
    ok, err = handler.performRequests_error_([req], None)

    if not ok or err is not None:
        return []

    results: list[QrAnnotation] = []
    for obs in req.results() or []:
        # Axis-aligned bounding box  → Stage 1 label
        bb = obs.boundingBox()
        bbox = vision_to_coco(bb.origin.x, bb.origin.y, bb.size.width, bb.size.height, img_w, img_h)

        # Four corner points → Stage 2 label
        corners: Optional[Corners] = None
        try:
            tl = obs.topLeft()
            tr = obs.topRight()
            br = obs.bottomRight()
            bl = obs.bottomLeft()
            tl_x, tl_y = vision_point_to_coco(tl.x, tl.y, img_w, img_h)
            tr_x, tr_y = vision_point_to_coco(tr.x, tr.y, img_w, img_h)
            br_x, br_y = vision_point_to_coco(br.x, br.y, img_w, img_h)
            bl_x, bl_y = vision_point_to_coco(bl.x, bl.y, img_w, img_h)
            corners = Corners(tl_x, tl_y, tr_x, tr_y, br_x, br_y, bl_x, bl_y)
        except Exception:
            pass

        results.append(QrAnnotation(bbox=bbox, corners=corners))

    return results


def get_image_dimensions(image_data: bytes) -> tuple[int, int]:
    """Return (width, height) of image without full decode."""
    ns_data = NSData.dataWithBytes_length_(image_data, len(image_data))
    source  = Quartz.CGImageSourceCreateWithData(ns_data, None)
    if source is None:
        return 0, 0
    props = Quartz.CGImageSourceCopyPropertiesAtIndex(source, 0, None)
    if props is None:
        return 0, 0
    w = props.get(Quartz.kCGImagePropertyPixelWidth,  0)
    h = props.get(Quartz.kCGImagePropertyPixelHeight, 0)
    return int(w), int(h)

# ---------------------------------------------------------------------------
# COCO builder (thread-safe)
# ---------------------------------------------------------------------------

class CocoBuilder:
    def __init__(self, output_dir: Path) -> None:
        self.output_dir = output_dir
        self.output_dir.mkdir(parents=True, exist_ok=True)

        self._lock = threading.Lock()

        # Three splits, each with its own accumulator
        self._splits: dict[str, dict] = {
            split: self._empty_coco() for split in ("train", "val", "test")
        }
        self._image_id   = 0
        self._ann_id     = 0
        self._total      = 0

    @staticmethod
    def _empty_coco() -> dict:
        return {
            "info": {
                "description": "rustslinger QR + telecom dataset",
                "version": "1.0",
                "year": 2026,
                "contributor": "tke1973",
                "date_created": time.strftime("%Y/%m/%d"),
            },
            "licenses":    [],
            "categories":  CATEGORIES,
            "images":      [],
            "annotations": [],
        }

    def add(self, record: ImageRecord) -> None:
        with self._lock:
            self._image_id += 1
            image_id = self._image_id
            split    = assign_split(record.sha256)
            coco     = self._splits[split]

            coco["images"].append({
                "id":        image_id,
                "file_name": record.s3_key,
                "width":     record.width,
                "height":    record.height,
                "sha256":    record.sha256,
            })

            # QR code annotations (bbox + keypoints)
            for qr in record.qr_anns:
                self._ann_id += 1
                area = round(qr.bbox.w * qr.bbox.h, 2)
                ann: dict = {
                    "id":          self._ann_id,
                    "image_id":    image_id,
                    "category_id": CAT_QR,
                    "bbox":        [qr.bbox.x, qr.bbox.y, qr.bbox.w, qr.bbox.h],
                    "area":        area,
                    "iscrowd":     0,
                }
                if qr.corners:
                    c = qr.corners
                    # keypoints: [x, y, visibility=2 (labeled+visible)] × 4
                    ann["keypoints"] = [
                        c.tl_x, c.tl_y, 2,
                        c.tr_x, c.tr_y, 2,
                        c.br_x, c.br_y, 2,
                        c.bl_x, c.bl_y, 2,
                    ]
                    ann["num_keypoints"] = 4
                coco["annotations"].append(ann)

            # Telecom equipment annotations (whole-image bbox, image-level label)
            for tl in record.tel_labels:
                self._ann_id += 1
                coco["annotations"].append({
                    "id":          self._ann_id,
                    "image_id":    image_id,
                    "category_id": tl.category_id,
                    "bbox":        [0, 0, record.width, record.height],
                    "area":        float(record.width * record.height),
                    "iscrowd":     0,
                    "label_confidence": tl.confidence,
                    # Whole-image bbox — not suitable for detection training without
                    # further grounded detection. Set iscrowd=0 but mark for filtering.
                    "whole_image_label": True,
                })

            self._total += 1

        if self._total % CHECKPOINT_EVERY == 0:
            self.save(checkpoint=True)

    def save(self, checkpoint: bool = False) -> None:
        suffix = ".checkpoint" if checkpoint else ""
        with self._lock:
            splits_snapshot = {k: json.loads(json.dumps(v)) for k, v in self._splits.items()}
            total = self._total

        for split, coco in splits_snapshot.items():
            dst = self.output_dir / f"{split}{suffix}.json"
            tmp = dst.with_suffix(".tmp")
            tmp.write_text(json.dumps(coco, indent=2), encoding="utf-8")
            tmp.rename(dst)   # POSIX atomic rename

        print(f"  [checkpoint] wrote {total} images across train/val/test", flush=True)

# ---------------------------------------------------------------------------
# Worker threads
# ---------------------------------------------------------------------------

def _sha256_bytes(data: bytes) -> str:
    h = hashlib.sha256(data)
    return h.hexdigest()


def download_worker(
    s3_client,
    bucket: str,
    key_queue:   "queue.Queue[object]",
    image_queue: "queue.Queue[object]",
    num_vision_workers: int,
    sentinel_count: list[int],
    sentinel_lock:  threading.Lock,
) -> None:
    """Download S3 objects and push raw bytes + metadata into image_queue."""
    while True:
        item = key_queue.get()
        if item is SENTINEL:
            # One download worker has finished. When all finish, send sentinels to Vision.
            with sentinel_lock:
                sentinel_count[0] += 1
                if sentinel_count[0] == DOWNLOAD_WORKERS:
                    for _ in range(num_vision_workers):
                        image_queue.put(SENTINEL)
            key_queue.task_done()
            return

        s3_key, meta = item
        try:
            resp = s3_client.get_object(Bucket=bucket, Key=s3_key)
            data = resp["Body"].read()
            image_queue.put((s3_key, meta, data))
        except Exception as exc:
            print(f"  [download] ERROR {s3_key}: {exc}", flush=True)
        finally:
            key_queue.task_done()


def vision_worker(
    image_queue:  "queue.Queue[object]",
    result_queue: "queue.Queue[object]",
    num_result_workers: int,
    sentinel_count: list[int],
    sentinel_lock:  threading.Lock,
) -> None:
    """Run Apple Vision QR detection and telecom label extraction on each image."""
    while True:
        item = image_queue.get()
        if item is SENTINEL:
            with sentinel_lock:
                sentinel_count[0] += 1
                if sentinel_count[0] == VISION_WORKERS:
                    for _ in range(num_result_workers):
                        result_queue.put(SENTINEL)
            image_queue.task_done()
            return

        s3_key, meta, data = item
        try:
            sha256 = _sha256_bytes(data)
            img_w, img_h = get_image_dimensions(data)

            if img_w == 0 or img_h == 0:
                image_queue.task_done()
                continue

            qr_anns    = detect_qr(data, img_w, img_h)
            tel_labels = resolve_labels(s3_key, meta)

            record = ImageRecord(
                s3_key=s3_key,
                data=b"",          # don't carry raw bytes forward
                sha256=sha256,
                width=img_w,
                height=img_h,
                qr_anns=qr_anns,
                tel_labels=tel_labels,
            )
            result_queue.put(record)
        except Exception as exc:
            print(f"  [vision] ERROR {s3_key}: {exc}", flush=True)
        finally:
            image_queue.task_done()


def result_worker(
    result_queue: "queue.Queue[object]",
    builder:      CocoBuilder,
    sentinel_count: list[int],
    sentinel_lock:  threading.Lock,
    done_event:     threading.Event,
    num_workers:    int,
) -> None:
    """Consume ImageRecords and add them to the COCO builder."""
    while True:
        item = result_queue.get()
        if item is SENTINEL:
            with sentinel_lock:
                sentinel_count[0] += 1
                if sentinel_count[0] == num_workers:
                    done_event.set()
            result_queue.task_done()
            return

        try:
            builder.add(item)
        except Exception as exc:
            print(f"  [result] ERROR: {exc}", flush=True)
        finally:
            result_queue.task_done()

# ---------------------------------------------------------------------------
# S3 key enumerator
# ---------------------------------------------------------------------------

def list_s3_keys(s3_client, bucket: str, prefix: Optional[str]) -> list[tuple[str, dict]]:
    """Return list of (key, metadata) tuples for all objects under prefix."""
    paginator = s3_client.get_paginator("list_objects_v2")
    kwargs    = {"Bucket": bucket}
    if prefix:
        kwargs["Prefix"] = prefix

    keys: list[tuple[str, dict]] = []
    print(f"Listing objects in s3://{bucket}/{prefix or ''} ...", flush=True)

    for page in paginator.paginate(**kwargs):
        for obj in page.get("Contents", []):
            key = obj["Key"]
            # Skip non-image files
            if not key.lower().endswith((".jpg", ".jpeg", ".png", ".webp", ".heic", ".heif")):
                continue
            # Fetch metadata lazily — head_object is cheap but slow at scale.
            # We collect keys first, then fetch metadata in download workers.
            keys.append((key, {}))

    print(f"Found {len(keys):,} image objects.", flush=True)
    return keys


def enrich_metadata(s3_client, bucket: str, key: str) -> dict:
    """Fetch S3 object metadata (HTTP HEAD)."""
    try:
        resp = s3_client.head_object(Bucket=bucket, Key=key)
        return resp.get("Metadata", {})
    except Exception:
        return {}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(
        description=(
            "Generate COCO-format QR + telecom training data from an S3 bucket.\n"
            "Run on macOS (AWS mac2.metal or Apple Silicon)."
        )
    )
    parser.add_argument("-b", "--bucket",  required=True,  help="AWS S3 bucket name")
    parser.add_argument("-p", "--prefix",  default=None,   help="S3 key prefix (optional)")
    parser.add_argument("-f", "--profile", default=None,   help="AWS profile name")
    parser.add_argument("-o", "--output",  default="dataset", help="Output directory for COCO JSON")
    parser.add_argument(
        "--download-workers", type=int, default=DOWNLOAD_WORKERS,
        help=f"S3 download worker count (default: {DOWNLOAD_WORKERS})",
    )
    parser.add_argument(
        "--vision-workers", type=int, default=VISION_WORKERS,
        help=f"Apple Vision worker count (default: {VISION_WORKERS})",
    )
    parser.add_argument(
        "--result-workers", type=int, default=4,
        help="Result writer worker count (default: 4)",
    )
    parser.add_argument(
        "--limit", type=int, default=None,
        help="Stop after processing this many images (for testing)",
    )
    args = parser.parse_args()

    # AWS client
    session_kwargs: dict = {}
    if args.profile:
        session_kwargs["profile_name"] = args.profile
    session   = boto3.Session(**session_kwargs)
    s3_client = session.client("s3")

    # Enumerate keys
    keys = list_s3_keys(s3_client, args.bucket, args.prefix)
    if args.limit:
        keys = keys[: args.limit]
        print(f"Limiting to {args.limit} images.", flush=True)

    if not keys:
        print("No images found. Exiting.")
        return

    output_dir = Path(args.output)
    builder    = CocoBuilder(output_dir)

    # Queues
    key_queue    : "queue.Queue[object]" = queue.Queue(maxsize=len(keys) + args.download_workers)
    image_queue  : "queue.Queue[object]" = queue.Queue(maxsize=QUEUE_MAXSIZE)
    result_queue : "queue.Queue[object]" = queue.Queue(maxsize=QUEUE_MAXSIZE)

    # Enqueue all keys
    for key, meta in keys:
        key_queue.put((key, meta))
    for _ in range(args.download_workers):
        key_queue.put(SENTINEL)

    # Sentinel tracking
    dl_sentinel_count:  list[int] = [0]
    vis_sentinel_count: list[int] = [0]
    res_sentinel_count: list[int] = [0]
    dl_sentinel_lock    = threading.Lock()
    vis_sentinel_lock   = threading.Lock()
    res_sentinel_lock   = threading.Lock()
    done_event          = threading.Event()

    # Spawn workers
    threads: list[threading.Thread] = []

    for _ in range(args.download_workers):
        t = threading.Thread(
            target=download_worker,
            args=(
                s3_client, args.bucket,
                key_queue, image_queue,
                args.vision_workers,
                dl_sentinel_count, dl_sentinel_lock,
            ),
            daemon=True,
        )
        t.start()
        threads.append(t)

    for _ in range(args.vision_workers):
        t = threading.Thread(
            target=vision_worker,
            args=(
                image_queue, result_queue,
                args.result_workers,
                vis_sentinel_count, vis_sentinel_lock,
            ),
            daemon=True,
        )
        t.start()
        threads.append(t)

    for _ in range(args.result_workers):
        t = threading.Thread(
            target=result_worker,
            args=(
                result_queue, builder,
                res_sentinel_count, res_sentinel_lock,
                done_event, args.result_workers,
            ),
            daemon=True,
        )
        t.start()
        threads.append(t)

    # Progress reporting
    print(f"Processing {len(keys):,} images with "
          f"{args.download_workers} download / {args.vision_workers} vision / "
          f"{args.result_workers} result workers ...", flush=True)

    start_time = time.time()
    last_count = 0

    while not done_event.wait(timeout=10.0):
        with builder._lock:
            current = builder._total
        delta    = current - last_count
        elapsed  = time.time() - start_time
        rate     = current / elapsed if elapsed > 0 else 0
        eta_secs = (len(keys) - current) / rate if rate > 0 else 0
        print(
            f"  {current:>8,} / {len(keys):,} images "
            f"({rate:.0f}/s, ETA {eta_secs/60:.1f} min)",
            flush=True,
        )
        last_count = current

    for t in threads:
        t.join()

    # Final save
    builder.save(checkpoint=False)

    elapsed = time.time() - start_time
    with builder._lock:
        total = builder._total
    print(
        f"\nDone. {total:,} images processed in {elapsed/60:.1f} minutes "
        f"({total/elapsed:.0f} img/s).",
        flush=True,
    )
    for split in ("train", "val", "test"):
        path = output_dir / f"{split}.json"
        with builder._lock:
            n_images = len(builder._splits[split]["images"])
            n_anns   = len(builder._splits[split]["annotations"])
        print(f"  {split:5s}: {n_images:>7,} images, {n_anns:>8,} annotations → {path}")


if __name__ == "__main__":
    main()
