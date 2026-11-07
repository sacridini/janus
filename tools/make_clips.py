#!/usr/bin/env python3
"""The three looping videos of the home page's "three steps" (site/assets/clips/<clip>.mp4, .webm, .jpg).

The frames come from the program itself: `janus --shots clip-explore|clip-detect|clip-prove IN OUT` drives the
program like the guides' tours (crates/janus-app/src/shots_clips.rs, shots_validation.rs), but saves every frame of
chosen stretches at 30 per second of the clip's own time, whatever the time each frame takes to draw, as
OUT/<clip>/frame_00001.png...; the pointer is the tour's own (injected events, never the user's mouse) and the app
draws it. This script runs the tours (one at a time, 4 threads) and turns the frames into the videos with ffmpeg
(4 threads): H.264 (yuv420p, faststart, CRF 28) and VP9, 1280 px wide, no audio, a poster, and the end fading into
the start so that the loop does not jump.

    # the data of the tours: the annual NDVI stack (explore, detect) and the validation tutorial's (prove)
    python tools/make_tutorial_data.py data                    # once; target/tutorial-validation has it
    # the whole of it (janus built with `cargo build --release -p janus-app --bin janus`)
    python tools/make_clips.py --stack C:/Users/eduar/Downloads/LT_Stack_NDVI_Rondonia.tif \\
        --validation target/tutorial-validation --work target/clips --out site/assets/clips
    # one clip, or the videos again from frames already made
    python tools/make_clips.py --only explore --stack ... --work target/clips --out site/assets/clips
    python tools/make_clips.py --only explore --no-record --work target/clips --out site/assets/clips

The window is 1680 x 1050; explore and detect are cropped to the map, the chart and the tool (the left column of
Layers, Files and Display is left out, so the text is larger at 1280 px), prove shows the whole window. The
lengths are in crates/janus-app/src/shots_clips.rs (explore 9.6 s, detect 10.9 s with the run's wait cut,
prove 9 s).
"""

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

FFMPEG = (
    r"C:\Users\eduar\AppData\Local\Microsoft\WinGet\Packages\Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe"
    r"\ffmpeg-9.0.2-full_build\bin\ffmpeg.exe"
)
FPS = 30
# per clip: the tour, the crop (x, y, w, h) of the 1680 x 1050 frames (None: all), the poster's frame (seconds
# into the clip), the frames of the end that fade into the start
CLIPS = {
    "explore": dict(tour="clip-explore", crop=(265, 34, 1415, 996), poster=6.0, fade=12, vp9="48"),
    "detect": dict(tour="clip-detect", crop=(265, 34, 1415, 996), poster=2.5, fade=12),
    "prove": dict(tour="clip-prove", crop=None, poster=1.2, fade=12),
}


def run(cmd, **kw):
    print("+", " ".join(str(c) for c in cmd), flush=True)
    subprocess.run([str(c) for c in cmd], check=True, **kw)


def record(clip, args):
    spec = CLIPS[clip]
    exe = Path(args.janus)
    data = args.validation if clip == "prove" else args.stack
    out = Path(args.work).resolve()
    out.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, RAYON_NUM_THREADS="4", GDAL_NUM_THREADS="1", OMP_NUM_THREADS="4")
    run([exe, "--threads", "4", "--shots", spec["tour"], Path(data).resolve(), out], env=env)


def encode(clip, args):
    spec = CLIPS[clip]
    frames = Path(args.work) / clip
    n = len(list(frames.glob("frame_*.png")))
    if n == 0:
        sys.exit(f"no frames in {frames}")
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    f = spec["fade"]
    crop = "crop={}:{}:{}:{},".format(*spec["crop"][2:], *spec["crop"][:2]) if spec["crop"] else ""
    # the last `f` frames dissolve into the first `f`, which then lead the loop's next round
    graph = (
        f"[0:v]{crop}scale=1280:-2:flags=lanczos,format=yuv420p,split=3[a][b][c];"
        f"[a]trim=start_frame={f}:end_frame={n - f},setpts=PTS-STARTPTS[m];"
        f"[b]trim=start_frame={n - f}:end_frame={n},setpts=PTS-STARTPTS[t];"
        f"[c]trim=start_frame=0:end_frame={f},setpts=PTS-STARTPTS[h];"
        f"[t][h]xfade=transition=fade:duration={f / FPS - 1 / FPS:.4f}:offset=0[x];"
        f"[m][x]concat=n=2:v=1:a=0,fps={FPS}[v]"
    )
    src = ["-y", "-hide_banner", "-loglevel", "error", "-threads", "4", "-framerate", FPS, "-i", frames / "frame_%05d.png"]
    common = ["-filter_complex", graph, "-map", "[v]", "-an"]
    run([FFMPEG, *src, *common, "-c:v", "libx264", "-preset", "veryslow", "-crf", args.crf, "-pix_fmt", "yuv420p",
         "-movflags", "+faststart", "-threads", "4", out / f"{clip}.mp4"])
    run([FFMPEG, *src, *common, "-c:v", "libvpx-vp9", "-b:v", "0", "-crf", spec.get("vp9", args.crf_vp9), "-row-mt", "1",
         "-cpu-used", "1", "-pix_fmt", "yuv420p", "-threads", "4", out / f"{clip}.webm"])
    k = min(n, max(1, int(spec["poster"] * FPS)))
    run([FFMPEG, "-y", "-hide_banner", "-loglevel", "error", "-i", frames / f"frame_{k:05d}.png", "-vf",
         f"{crop}scale=1280:-2:flags=lanczos", "-q:v", "5", out / f"{clip}.jpg"])
    for ext in ("mp4", "webm", "jpg"):
        p = out / f"{clip}.{ext}"
        print(f"  {p}: {p.stat().st_size / 1e6:.2f} MB")
    print(f"  {n} frames, {n / FPS:.1f} s of recording")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--janus", default="target/release/janus.exe")
    ap.add_argument("--stack", help="the annual NDVI stack of Rondonia (explore, detect)")
    ap.add_argument("--validation", default="target/tutorial-validation", help="the validation tutorial's data (prove)")
    ap.add_argument("--work", default="target/clips")
    ap.add_argument("--out", default="site/assets/clips")
    ap.add_argument("--only", choices=list(CLIPS))
    ap.add_argument("--no-record", action="store_true", help="only the videos, from the frames in --work")
    ap.add_argument("--crf", default="28")
    ap.add_argument("--crf-vp9", default="38")
    args = ap.parse_args()
    if shutil.which(FFMPEG) is None and not Path(FFMPEG).exists():
        sys.exit("ffmpeg not found: set FFMPEG in this script")
    for clip in [args.only] if args.only else list(CLIPS):
        if not args.no_record:
            record(clip, args)
        encode(clip, args)


if __name__ == "__main__":
    main()
