"""Generates short test tones (mp3, ogg, flac, wav) for trying the player.

Usage: python3 tools/make_test_audio.py [output-folder]   (default: test-audio)
"""

import subprocess
import sys
from pathlib import Path

out = Path(sys.argv[1] if len(sys.argv) > 1 else "test-audio")
album = out / "Sample Album"
album.mkdir(parents=True, exist_ok=True)

codecs = [("mp3", ["-b:a", "96k"]), ("ogg", ["-c:a", "libvorbis"]), ("flac", ["-c:a", "flac"])]
freqs = [220, 262, 330, 392, 440, 523, 659, 784, 880, 988, 1047, 1175]
for number, freq in enumerate(freqs, start=1):
    ext, args = codecs[number % 3]
    target = album / f"{number:02d} Tone {freq}.{ext}"
    subprocess.run(
        ["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi",
         "-i", f"sine=frequency={freq}:duration={20 + number * 7}", "-ac", "2",
         "-metadata", f"title=Tone {number:02d} ({freq} Hz)",
         "-metadata", "artist=Test Signals", "-metadata", "album=Sample Album",
         *args, str(target)],
        check=True,
    )

subprocess.run(
    ["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=500:duration=3",
     str(out / "single.wav")],
    check=True,
)
print("\n".join(sorted(str(p) for p in out.rglob("*") if p.is_file())))
