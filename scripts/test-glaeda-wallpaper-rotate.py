#!/usr/bin/env python3
import json
import os
import plistlib
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/glaeda-wallpaper-rotate"


class WallpaperRotateTests(unittest.TestCase):
    def test_rotates_image_choices_and_advances_state(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            store = home / "Library/Application Support/com.apple.wallpaper/Store"
            store.mkdir(parents=True)
            images = root / "wallpapers"
            images.mkdir()
            (images / "02-second.jpg").write_bytes(b"second")
            (images / "01-first.jpg").write_bytes(b"first")
            document = {
                "AllSpacesAndDisplays": {"Linked": {"Content": {"Choices": [{"Provider": "aerial"}]}}},
                "SystemDefault": {"Desktop": {"Content": {"Choices": [{"Provider": "image"}]}}},
                "Displays": {"display-1": {"Desktop": {"Content": {"Choices": [{"Provider": "image"}]}}}},
                "Spaces": {"space-1": {"Default": {"Desktop": {"Content": {"Choices": [{"Provider": "image"}]}}}}},
            }
            plist = store / "Index.plist"
            plist.write_bytes(plistlib.dumps(document, fmt=plistlib.FMT_BINARY))
            state = home / "state/wallpaper.json"
            env = {
                **os.environ,
                "HOME": str(home),
                "GLAEDA_WALLPAPER_DIR": str(images),
                "GLAEDA_WALLPAPER_STATE": str(state),
            }
            first = subprocess.run([str(SCRIPT)], env=env, text=True, capture_output=True, check=True)
            self.assertIn("01-first.jpg (4 spaces)", first.stdout)
            updated = plistlib.loads(plist.read_bytes())
            for name in ("AllSpacesAndDisplays", "SystemDefault"):
                space = updated[name]
                self.assertEqual(space["Type"], "individual")
                content = space["Desktop"]["Content"]
                self.assertEqual(content["Choices"][0]["Provider"], "com.apple.wallpaper.choice.image")
                self.assertTrue(content["Choices"][0]["Files"][0]["relative"].endswith("01-first.jpg"))
                self.assertNotEqual(content["EncodedOptionValues"], "$null")
            self.assertTrue((state.parent / "Index.plist.previous").is_file())
            self.assertEqual(json.loads(state.read_text())["index"], 1)

            second = subprocess.run([str(SCRIPT)], env=env, text=True, capture_output=True, check=True)
            self.assertIn("02-second.jpg", second.stdout)
            self.assertEqual(json.loads(state.read_text())["index"], 2)

    def test_missing_store_and_images_are_successful_noops(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            env = {**os.environ, "HOME": str(root), "GLAEDA_WALLPAPER_DIR": str(root / "none")}
            result = subprocess.run([str(SCRIPT)], env=env, text=True, capture_output=True, check=True)
            self.assertIn("no GUI wallpaper store", result.stdout)


if __name__ == "__main__":
    unittest.main()
