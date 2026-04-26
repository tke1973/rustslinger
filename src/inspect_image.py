#!/usr/bin/env python3
#
# Copyright Thomas Kessler <tom@kessler.group> All Rights Reserved.
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Usage: python inspect_image.py <image_path>

import hashlib
import json
import sys
from pathlib import Path

import Quartz
import Vision
from Foundation import NSData


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def load_cg_image(data: bytes):
    ns   = NSData.dataWithBytes_length_(data, len(data))
    src  = Quartz.CGImageSourceCreateWithData(ns, None)
    return Quartz.CGImageSourceCreateImageAtIndex(src, 0, None)


def dimensions(cg) -> tuple[int, int]:
    return Quartz.CGImageGetWidth(cg), Quartz.CGImageGetHeight(cg)


def to_coco_point(px: float, py: float, img_w: int, img_h: int) -> tuple[float, float]:
    """Normalised Vision point → absolute COCO pixels (Y-axis flip included)."""
    return round(px * img_w, 2), round((1.0 - py) * img_h, 2)


def detect(data: bytes) -> dict:
    cg = load_cg_image(data)
    if cg is None:
        sys.exit("ERROR: could not decode image")

    w, h = dimensions(cg)

    req     = Vision.VNDetectBarcodesRequest.alloc().init()
    req.setSymbologies_([Vision.VNBarcodeSymbologyQR])
    handler = Vision.VNImageRequestHandler.alloc().initWithCGImage_options_(cg, {})
    ok, err = handler.performRequests_error_([req], None)

    if not ok or err:
        sys.exit(f"ERROR: Vision request failed: {err}")

    annotations = []
    for obs in req.results() or []:
        bb   = obs.boundingBox()
        vx, vy, vw, vh = bb.origin.x, bb.origin.y, bb.size.width, bb.size.height

        # bbox: axis-aligned, absolute pixels, COCO origin (top-left)
        bbox = [
            round(vx * w, 2),
            round((1.0 - vy - vh) * h, 2),
            round(vw * w, 2),
            round(vh * h, 2),
        ]

        # keypoints: 4 corners [x, y, visibility=2] in TL→TR→BR→BL order
        pts     = [obs.topLeft(), obs.topRight(), obs.bottomRight(), obs.bottomLeft()]
        kp_flat = [c for p in pts for c in (*to_coco_point(p.x, p.y, w, h), 2)]

        annotations.append({
            "qr_content":     obs.payloadStringValue(),
            "bbox":           bbox,
            "area":           round(bbox[2] * bbox[3], 2),
            "keypoints":      kp_flat,
            "num_keypoints":  4,
            "iscrowd":        0,
        })

    return {
        "image":  {"width": w, "height": h, "sha256": sha256(data)},
        "annotations": annotations,
    }


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("Usage: inspect_image.py <image_path>")

    path = Path(sys.argv[1])
    if not path.exists():
        sys.exit(f"ERROR: file not found: {path}")

    result = detect(path.read_bytes())
    print(json.dumps(result, indent=2))
