#!/usr/bin/env python3
"""Validate AppStream screenshot URLs against their committed PNG sources."""

from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path
from urllib.parse import urlparse
from xml.etree import ElementTree

UPSTREAM_PREFIX = "/rayan6ms/noire/main/"
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


def png_dimensions(path: Path) -> tuple[int, int]:
    with path.open("rb") as screenshot:
        header = screenshot.read(24)
    if len(header) != 24 or header[:8] != PNG_SIGNATURE or header[12:16] != b"IHDR":
        raise ValueError(f"screenshot is not a valid PNG: {path}")
    return struct.unpack(">II", header[16:24])


def validate(metadata: Path, repository: Path) -> int:
    component = ElementTree.parse(metadata).getroot()
    screenshots = component.findall("./screenshots/screenshot")
    if not screenshots:
        raise ValueError("AppStream metadata has no screenshots")
    if sum(item.get("type") == "default" for item in screenshots) != 1:
        raise ValueError("AppStream metadata must have exactly one default screenshot")

    checked = 0
    for screenshot in screenshots:
        caption = screenshot.findtext("caption", default="").strip()
        if not caption:
            raise ValueError("every AppStream screenshot needs a caption")
        images = screenshot.findall("image")
        sources = [image for image in images if image.get("type") == "source"]
        if len(sources) != 1:
            raise ValueError("every screenshot needs exactly one source image")
        image = sources[0]
        url = (image.text or "").strip()
        parsed = urlparse(url)
        if parsed.scheme != "https" or parsed.netloc != "raw.githubusercontent.com":
            raise ValueError(f"screenshot must use the upstream HTTPS raw URL: {url}")
        if not parsed.path.startswith(UPSTREAM_PREFIX):
            raise ValueError(f"screenshot URL is outside the Noire main branch: {url}")
        relative = parsed.path.removeprefix(UPSTREAM_PREFIX)
        local = (repository / relative).resolve()
        try:
            local.relative_to(repository.resolve())
        except ValueError as error:
            raise ValueError(f"screenshot URL escapes the repository: {url}") from error
        if not local.is_file():
            raise ValueError(f"screenshot URL has no committed source file: {relative}")
        width, height = png_dimensions(local)
        if image.get("width") != str(width) or image.get("height") != str(height):
            raise ValueError(
                f"declared dimensions do not match {relative}: "
                f"{image.get('width')}x{image.get('height')} != {width}x{height}"
            )
        if width * 9 != height * 16:
            raise ValueError(f"AppStream source screenshot is not 16:9: {relative}")
        if width < 1280 or height < 720:
            raise ValueError(f"AppStream source screenshot is smaller than 1280x720: {relative}")
        checked += 1

    print(f"NOIRE_APPSTREAM_SCREENSHOTS count={checked} local=pass dimensions=pass ratio=16:9")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("metadata", type=Path)
    parser.add_argument("repository", type=Path)
    arguments = parser.parse_args()
    try:
        return validate(arguments.metadata, arguments.repository)
    except (ElementTree.ParseError, OSError, ValueError) as error:
        print(f"AppStream screenshot validation failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
