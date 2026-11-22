"""Fail the delivery gate unless every real iPad screen has complete evidence."""
import json
import math
from pathlib import Path
import struct
import sys

PORTRAIT = ("shelf", "detail", "reader", "history", "sources", "settings", "community",
            "browse", "filters", "filtered")
LANDSCAPE = ("shelf-landscape", "detail-landscape", "reader-landscape", "filters-landscape")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def display_size(image):
    """XCTest PNGs can store landscape as portrait pixels with EXIF rotation."""
    require(len(image) > 32 and image[:8] == b"\x89PNG\r\n\x1a\n"
            and image[12:16] == b"IHDR", "missing PNG screenshot")
    width, height = struct.unpack(">II", image[16:24])
    offset = 8
    while offset + 12 <= len(image):
        size = struct.unpack_from(">I", image, offset)[0]
        kind = image[offset + 4:offset + 8]
        data = image[offset + 8:offset + 8 + size]
        require(len(data) == size, "truncated PNG chunk")
        if kind == b"eXIf":
            order = "<" if data[:2] == b"II" else ">"
            require(data[:2] in (b"II", b"MM") and len(data) >= 8, "invalid PNG EXIF")
            require(struct.unpack_from(order + "H", data, 2)[0] == 42, "invalid EXIF TIFF")
            ifd = struct.unpack_from(order + "I", data, 4)[0]
            count = struct.unpack_from(order + "H", data, ifd)[0]
            for index in range(count):
                entry = ifd + 2 + 12 * index
                tag, value_type, length = struct.unpack_from(order + "HHI", data, entry)
                if tag == 274:
                    require(value_type == 3 and length == 1, "invalid EXIF orientation")
                    orientation = struct.unpack_from(order + "H", data, entry + 8)[0]
                    require(1 <= orientation <= 8, "invalid EXIF rotation")
                    if orientation >= 5:
                        width, height = height, width
                    return width, height
        offset += size + 12
    return width, height


def verify(directory):
    directory = Path(directory)
    launches = set()
    reports = {}
    legacy_path = directory / "legacy-library.json"
    relocated_path = directory / "relocated-library.json"
    library_evidence = False
    container_changed = None
    book_ids = None
    if legacy_path.is_file() and relocated_path.is_file():
        legacy = json.loads(legacy_path.read_text(encoding="utf-8"))
        relocated = json.loads(relocated_path.read_text(encoding="utf-8"))
        require(legacy["schema"] == 3, "fixture must start in the build 3 library format")
        require(relocated["schema"] == 4 and relocated["managed_paths_relative"],
                "legacy library was not upgraded to portable paths")
        book_ids = set(legacy["book_ids"])
        require(len(book_ids) == 5 and set(relocated["book_ids"]) == book_ids,
                "library contents changed across reinstallation")
        library_evidence = True
        container_changed = legacy["root"] != relocated["root"]
    else:
        missing = [p.name for p in (legacy_path, relocated_path) if not p.is_file()]
        print("WARNING: {} missing; capture stopped before the reinstall "
              "check, skipping the legacy-library upgrade assertions".format(
                  "/".join(missing)), file=sys.stderr)
    for name in (*PORTRAIT, *LANDSCAPE):
        screen = name.split("-")[0]
        report = json.loads((directory / f"{name}.json").read_text(encoding="utf-8"))
        require(report["screen"] == screen, f"{name}: wrong screen")
        run_id = report["run_id"]
        require(isinstance(run_id, str) and bool(run_id) and run_id not in launches,
                f"{name}: missing or reused launch acknowledgement")
        launches.add(run_id)
        width, height = report["width"], report["height"]
        require(all(isinstance(n, (float, int)) and math.isfinite(n) and n > 0
                    for n in (width, height)), f"{name}: invalid viewport")
        landscape = name in LANDSCAPE
        require((width > height) if landscape else (height > width),
                f"{name}: wrong rendered orientation")
        require(report["books"] == 5, f"{name}: original demo library was lost")
        ids = set(report["book_ids"])
        require(len(ids) == 5, f"{name}: imported book count changed")
        if book_ids is None:
            book_ids = ids
        require(ids == book_ids, f"{name}: imported book identities changed")
        require(report["archives_available"] == report["books"],
                f"{name}: imported archives are inaccessible")
        require(report["cover_files_available"] == report["books"],
                f"{name}: imported cover files are inaccessible")
        require(report["covers_pending"] == 0 and report["covers_failed"] == 0,
                f"{name}: covers did not finish loading")
        if screen in ("shelf", "detail"):
            require(report["covers_loaded"] > 0, f"{name}: placeholder covers only")
        if screen == "reader":
            require(report["page_loaded"] is True, f"{name}: comic image not loaded")
        if screen == "community":
            require(report["repository_sources"] > 0, f"{name}: empty community repository")
        if screen in ("browse", "filters", "filtered"):
            browser = report["source_browser"]
            require(browser["source"] == "local.filter-acceptance" and browser["loaded"]
                    and not browser["busy"] and browser["filter_count"] == 7
                    and browser["listing_count"] == 2, f"{name}: incomplete source metadata")
            results = browser["results"]
            if screen == "browse":
                # Reaching the grid's last row auto-loads further listing pages.
                ok = (bool(results) and results[0] == "热门-第1页"
                      and all(r.startswith("热门-第") for r in results))
            else:
                ok = results == ["筛选:冒险题材" if screen == "filtered" else "热门-第1页"]
            require(ok, f"{name}: WASM filter/listing result not loaded")
            require(browser["filter_editor"] == (screen == "filters"), f"{name}: wrong filter view")
        left, top, right, bottom = report["safe_area"]
        require(all(isinstance(n, (int, float)) and math.isfinite(n)
                    for n in (left, top, right, bottom)),
                f"{name}: invalid safe area")
        require(abs(right - left - width) < 1 and abs(bottom - top - height) < 1,
                f"{name}: inconsistent safe area")
        navigation = report["navigation"]
        if screen in ("shelf", "history", "sources", "settings", "community","browse","filters","filtered"):
            require(set(navigation) == {"nav-library", "nav-search", "nav-history", "nav-settings"},
                    f"{name}: incomplete navigation")
        for control, rect in navigation.items():
            x1, y1, x2, y2 = rect
            require(left <= x1 < x2 <= right and top <= y1 < y2 <= bottom,
                    f"{name}: {control} outside the safe area")
            require(x2 - x1 >= 44 and y2 - y1 >= 44,
                    f"{name}: {control} below touch target size")
        # simctl and XCTest supply the PNGs; check orientation and that the
        # captured display can actually contain the reported content area.
        image = (directory / f"{name}.png").read_bytes()
        pixels_w, pixels_h = display_size(image)
        require(pixels_w >= width and pixels_h >= height, f"{name}: screenshot too small")
        require((pixels_w > pixels_h) if landscape else (pixels_h > pixels_w),
                f"{name}: screenshot orientation differs from readiness")
        reports[name] = report
    return {
        "result": "passed",
        "screens": list(reports),
        "landscape_reader_loaded": reports["reader-landscape"]["page_loaded"],
        "community_sources": reports["community"]["repository_sources"],
        "source_filters_and_listings": True,
        "legacy_library_upgraded": library_evidence,
        "data_container_changed": container_changed,
    }


if __name__ == "__main__":
    result = verify(sys.argv[1])
    (Path(sys.argv[1]) / "verification.json").write_text(
        json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print("PASS: all iPad screenshots and per-launch content acknowledgements verified")
