# Drops live interaction demo

The revised `../drops-live-demo.mp4` is 20 seconds, 1920×1080, 24 fps, H.264 + AAC. It uses continuous recorded UI interaction, edited with moving close-ups, feature captions, click rings and an original quiet soundtrack.

The take shows actual mouse and keyboard input: create a Drop, select Cat, drag it into a workspace, type a README request, receive Gemma 4's response, reopen the conversation, type a file-creation request, inspect its real diff and click Allow. The resulting file was verified to contain `Ready to ship with Drops.` Reasoning is shown in its native collapsed block.

Because the host desktop was locked, this ran in an isolated Xvfb display. The production QML controls, animations and chat were used with a display adapter that replaces Wayland layer surfaces with positioned floating windows and supplies pointer coordinates to Drops' supported headless daemon. Placement therefore uses a synthetic workspace; this video does not demonstrate actual window attachment or OCR. The launch-notes background is demo scenery. The approval text field was given a dark background in the recording adapter so the real diff is readable. Product source and user desktop configuration were not changed.

`raw-take.mp4` is the unedited recording and `marks.json` contains actual event times. `edit.py` rebuilds the final edit from those files. It requires Python with Pillow, ffmpeg, and the installed Noto Sans fonts. Run from any directory:

```sh
python3 demo/live/edit.py
```

Model waiting time is condensed. No model text, approval content or file result was fabricated. The small sound accents are editorial effects.
