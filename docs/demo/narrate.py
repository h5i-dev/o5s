#!/usr/bin/env python3
"""Generate scene-timed Kokoro narration and mix it to the film timeline."""
import json
import subprocess
import argparse
from pathlib import Path

import numpy as np
import soundfile as sf
from kokoro import KPipeline

HERE = Path(__file__).resolve().parent
SEGMENTS = json.loads((HERE / "narration.json").read_text())
WORK = HERE / "out" / "voice-clips"
DEST = HERE / "audio" / "narration.mp3"
SAMPLE_RATE = 24000


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scene", type=int, action="append", help="regenerate this 1-based scene; repeat to select more")
    selected = parser.parse_args().scene
    if selected is not None and any(not 1 <= scene <= len(SEGMENTS) for scene in selected):
        parser.error(f"--scene must be between 1 and {len(SEGMENTS)}")
    WORK.mkdir(parents=True, exist_ok=True)
    DEST.parent.mkdir(parents=True, exist_ok=True)
    pipeline = KPipeline(lang_code="a")
    clips = []
    tempo = []
    for i, segment in enumerate(SEGMENTS):
        clip = WORK / f"{i:02}.wav"
        if selected is None or i + 1 in selected:
            chunks = [np.asarray(audio, dtype=np.float32) for _, _, audio in
                      pipeline(segment["text"], voice="af_heart", speed=1.0)]
            if not chunks:
                raise RuntimeError(f"Scene {i + 1}: Kokoro returned no audio")
            gap = np.zeros(round(SAMPLE_RATE * .13), dtype=np.float32)
            voice = np.concatenate([part for j, chunk in enumerate(chunks)
                                    for part in ([gap, chunk] if j else [chunk])])
            sf.write(clip, voice, SAMPLE_RATE)
        else:
            voice, sample_rate = sf.read(clip)
            if sample_rate != SAMPLE_RATE:
                raise RuntimeError(f"Scene {i + 1}: clip has {sample_rate} Hz")
        clips.append(clip)
        duration = len(voice) / SAMPLE_RATE
        available = segment["end"] - segment["start"] - 2
        speed = max(1, duration / available)
        if speed > 1.25:
            raise RuntimeError(f"Scene {i + 1}: speech requires {speed:.2f}x; shorten copy")
        tempo.append(speed)
        print(f"scene {i + 1:02}: {duration:.1f}s / {available}s, tempo {speed:.2f}x", flush=True)

    args = ["ffmpeg", "-y", "-loglevel", "error"]
    for clip in clips:
        args += ["-i", str(clip)]
    filters = []
    for i, segment in enumerate(SEGMENTS):
        delay = (segment["start"] + 1) * 1000
        filters.append(f"[{i}:a]atempo={tempo[i]:.5f},aresample=48000,adelay={delay}|{delay}[a{i}]")
    filters.append("".join(f"[a{i}]" for i in range(len(clips))) +
                   f"amix=inputs={len(clips)}:duration=longest:normalize=0," +
                   "aresample=48000[out]")
    args += ["-filter_complex", ";".join(filters), "-map", "[out]", "-t", str(SEGMENTS[-1]["end"]),
             "-c:a", "libmp3lame", "-b:a", "128k", str(DEST)]
    subprocess.run(args, check=True)
    print(f"wrote {DEST}")


if __name__ == "__main__":
    main()
