#!/usr/bin/env python3
"""Check that a decoded desktop frame contains the smoke terminal's background."""
import sys
from PIL import Image

image = Image.open(sys.argv[1]).convert("RGB")
width, height = map(int, sys.argv[2].split("x"))
pixels = image.get_flattened_data()
matching = sum(max(abs(c - t) for c, t in zip(p, (22, 78, 99))) < 16 for p in pixels)
if image.size != (width, height) or matching < width * height // 10:
    sys.exit("Waiting for a desktop frame containing the teal terminal.")
