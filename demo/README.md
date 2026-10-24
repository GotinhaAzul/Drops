# Drops product demo

`drops-demo.mp4` is a 20-second, 1920×1080 H.264 video at 24 fps, with no audio.

The native QML interface was rendered offscreen from snapshots of an actual isolated Drops session using Gemma 4 via `localai` at localhost:8083. Gemma read a demo README, summarized it, requested approval to write `launch-note.txt`, and wrote `Ready to ship with Drops.` after approval. Export and fork were also executed successfully. No credentials or personal desktop content are included.

The video combines native interface renders, animated presentation frames, and capability captions. Window attachment, OCR, Office editing, reasoning and web access are described capabilities, not live demonstrations in this cut. The desktop was locked, so the captured session used a synthetic headless workspace. Model latency is condensed. This is a feature overview with a live-model example, rather than an exhaustive end-to-end recording of every feature.

Rebuild with `python3 demo/build_demo.py` (requires Pillow, ffmpeg and the installed Noto Sans fonts). Native UI source frames are in `assets/`. `poster.jpg` is a thumbnail and `storyboard.jpg` contains the six scenes for review.
