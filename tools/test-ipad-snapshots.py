"""Regression tests for the iPad evidence gate, using labelled synthetic reports."""
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest
import zlib

spec = importlib.util.spec_from_file_location(
    "ipad_snapshots", Path(__file__).with_name("verify-ipad-snapshots.py")
)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


def png(width, height):
    def chunk(name, data):
        return (struct.pack(">I", len(data)) + name + data
                + struct.pack(">I", zlib.crc32(name + data)))
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress((b"\0" * (width * 3 + 1)) * height))
            + chunk(b"IEND", b""))


class IpadGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        portrait = png(834, 1194)
        landscape = png(1194, 834)
        ids = [f"demo-{n}" for n in range(5)]
        self.write("legacy-library", {"schema": 3, "book_ids": ids, "root": "old"})
        self.write("relocated-library", {
            "schema": 4, "book_ids": ids, "root": "new", "managed_paths_relative": True
        })
        for name in (*gate.PORTRAIT, *gate.LANDSCAPE):
            screen = name.split("-")[0]
            width, height = (1194, 834) if name in gate.LANDSCAPE else (834, 1194)
            report = {
                "screen": screen, "run_id": f"synthetic-{name}", "books": 5,
                "book_ids": ids,
                "archives_available": 5, "cover_files_available": 5,
                "covers_pending": 0, "covers_failed": 0, "covers_loaded": 5,
                "page_loaded": screen == "reader", "repository_sources": 176,
                "width": width, "height": height,
                "safe_area": [0, 0, width, height],
                "navigation": {
                    label: [i * 60, height - 50, i * 60 + 50, height]
                    for i, label in enumerate((
                        "nav-library", "nav-search", "nav-history", "nav-settings"
                    ))
                },
            }
            self.write(name, report)
            (self.root / f"{name}.png").write_bytes(
                landscape if name in gate.LANDSCAPE else portrait
            )

    def write(self, name, report):
        (self.root / f"{name}.json").write_text(json.dumps(report), encoding="utf-8")

    def change(self, name, **changes):
        report = json.loads((self.root / f"{name}.json").read_text(encoding="utf-8"))
        report.update(changes)
        self.write(name, report)

    def test_complete_evidence_is_accepted(self):
        self.assertEqual(gate.verify(self.root)["result"], "passed")

    def test_missing_landscape_report_blocks_delivery(self):
        # Reproduces the old green run: screenshots/XCTest pass but no JSON.
        (self.root / "shelf-landscape.json").unlink()
        with self.assertRaises(FileNotFoundError):
            gate.verify(self.root)

    def test_loaded_content_is_required(self):
        for field, value in (("page_loaded", False), ("archives_available", 0),
                             ("cover_files_available", 0), ("covers_failed", 1)):
            with self.subTest(field=field):
                self.change("reader-landscape", **{field: value})
                with self.assertRaises(ValueError):
                    gate.verify(self.root)
                self.change("reader-landscape", page_loaded=True, archives_available=5,
                            cover_files_available=5, covers_failed=0)

    def test_reused_launch_report_is_rejected(self):
        self.change("shelf-landscape", run_id="synthetic-shelf")
        with self.assertRaisesRegex(ValueError, "reused"):
            gate.verify(self.root)

    def test_replaced_or_unmigrated_library_is_rejected(self):
        self.change("relocated-library", schema=3, managed_paths_relative=False)
        with self.assertRaisesRegex(ValueError, "upgraded"):
            gate.verify(self.root)
        self.change("relocated-library", schema=4, managed_paths_relative=True,
                    book_ids=["different"])
        with self.assertRaisesRegex(ValueError, "contents changed"):
            gate.verify(self.root)

    def test_wrong_screenshot_orientation_is_rejected(self):
        (self.root / "shelf-landscape.png").write_bytes(
            (self.root / "shelf.png").read_bytes()
        )
        with self.assertRaises(ValueError):
            gate.verify(self.root)

    def test_xctest_exif_landscape_is_accepted(self):
        # XCTest stores portrait pixel dimensions and orientation=6.
        tiff = b"MM" + struct.pack(">HIH", 42, 8, 1)
        tiff += struct.pack(">HHIHHI", 274, 3, 1, 6, 0, 0)
        kind = b"eXIf"
        chunk = (struct.pack(">I", len(tiff)) + kind + tiff
                 + struct.pack(">I", zlib.crc32(kind + tiff)))
        image = png(834, 1194)
        (self.root / "shelf-landscape.png").write_bytes(image[:33] + chunk + image[33:])
        self.assertEqual(gate.verify(self.root)["result"], "passed")

    def test_clipped_navigation_is_rejected(self):
        self.change("shelf", navigation={"nav-library": [0, 1200, 60, 1260],
                                       "nav-search": [0, 0, 60, 60],
                                       "nav-history": [60, 0, 120, 60],
                                       "nav-settings": [120, 0, 180, 60]})
        with self.assertRaisesRegex(ValueError, "safe area"):
            gate.verify(self.root)


if __name__ == "__main__":
    unittest.main()
