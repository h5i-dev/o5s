# o5s demo film — Eight arms, one channel

This is a **6:45 HTML film** for Cloudflare's [Build the next Git platform](https://blog.cloudflare.com/next-git-platform-on-cloudflare/) submission. It follows the octopus from the o5s UI through the problem, the patch model, the verified Rust core, and the Workers + Artifacts design. English Kokoro narration accompanies the octopus; the on-screen captions also carry the main points.

Open `index.html` in a browser and press **Play**. Use **Space** to pause, **←/→** to move five seconds, or the scrub bar. The music-note button mutes narration. Playback starts on a click so browsers can play audio. Every frame is a function of time exposed as `window.SEEK(milliseconds)`, so captures can be reproduced exactly.

## Render

`render.mjs` needs Node, ffmpeg, Playwright, and its Chromium headless shell. It looks for a local/global Playwright install or an npm cache, following the renderer in `../h5i/docs/demo`.

```sh
cd docs/demo
node render.mjs --stills 12,100,190,255,290,325,355
node render.mjs                           # 1080p, 24 fps -> out/o5s-demo.mp4
node render.mjs --scale 2 --out-height 1080  # supersampled 1080p
node render.mjs --fps 8 --width 960 --fast    # faster 540p draft
```

The MP4 includes the generated narration. To regenerate the voice track, install PyTorch, `kokoro`, and `soundfile` in a Python environment, then run `python narrate.py`. After changing one scene, `python narrate.py --scene 3` regenerates that clip and remixes the track. Kokoro downloads its model and `af_heart` voice on first use. The copy and scene timing live in `narration.json`; the output is `audio/narration.mp3`. `out/` contains temporary voice clips, stills, and MP4 renders and is ignored by Git.

## Timeline

| Time | Beat |
| --- | --- |
| 0:00–0:25 | Introduce the octopus and the 1,000-agent question. |
| 0:25–0:55 | Problem: a Git commit graph shows the moving branch and stale agent bases. |
| 0:55–1:25 | Problem: critical VCS vulnerabilities and agent attacks make correctness vital. |
| 1:25–2:25 | One immutable patch model: disjoint edits integrate; overlapping edits remain visible as a conflict. |
| 2:25–3:00 | Rust decision kernel, Lean 4 verification, and the 915-theorem proof tree. |
| 3:00–3:30 | Refinement, gate, convergence, and causal heads. |
| 3:30–4:05 | Worker, Hyperdrive/Postgres, and Artifacts. |
| 4:05–4:50 | Artifacts workspaces, Worker's tree diff, causal heads, and Git projection. |
| 4:50–5:40 | Real CLI commands animated beside the embedded o5s repository UI with sample state. |
| 5:40–6:10 | Recorded single-host Git comparison. |
| 6:10–6:45 | Source and candid scope of current assurance. |

## Evidence and claim boundaries

- The narrative and 1,000-agent result come from [`o5s-short-paper-v5.pdf`](../o5s-short-paper-v5.pdf) and the recorded [comparison data](../evidence/git-comparison/local-cmp1000/). The 574/s versus 9.8/s numbers are for different-line edits in one file with 1,000 agents and 64 in flight on one host. They are **not** Cloudflare or 10,000-client throughput numbers.
- The proof descriptions are grounded in [`proofs.md`](../proofs.md), especially `Guarantees.merge_gate`, `Refine.transition_refines`, `DocMatRefine.extracted_view_converges`, and `Heads.heads_keep_past`. The film summarizes their statements; proof assumptions and unproved adapters remain in that document.
- The Cloudflare deployment and its current limits are recorded in [`status.md`](../status.md). The Worker + Hyperdrive path has been exercised end to end; the live Artifacts workspace flow has not yet been exercised end to end. The diagram describes the implemented design, not a claim of fully verified distributed operation.
- The security beat references Git's [five 2024 vulnerabilities](https://github.blog/open-source/git/securing-git-addressing-5-new-vulnerabilities/) and [seven 2025 vulnerabilities](https://github.blog/open-source/git/git-security-vulnerabilities-announced-6/), [GitHub's 2026 push-pipeline disclosure](https://github.blog/security/securing-the-git-push-pipeline-responding-to-a-critical-remote-code-execution-vulnerability/), [OpenAI's Hugging Face incident report](https://openai.com/index/hugging-face-incident-and-the-road-ahead/), [Anthropic's cyber-evaluation incident report](https://www.anthropic.com/news/investigating-incidents-cybersecurity-evals), and [Irregular's evaluation-environment report](https://www.irregular.cv/research/addressing-recent-incidents-ongoing-findings-and-path-forward). Google confirmed the Gemini case in a [statement reported by the Associated Press](https://apnews.com/article/f590b51073d8977a984c7c42b8f2a7a2). The evaluation cases differ in cause; the film says only that agents reached real systems outside the intended test scope. GitHub reported no unauthorized exploitation of its disclosed flaw. The film makes no claim that o5s would have prevented these cases.
- The CLI commands in the viewer scene are real commands documented in [`cli.md`](../cli.md). The embedded viewer is generated from the actual [`crates/server/src/index.html`](../../crates/server/src/index.html) by `make_ui_replay.py`; it uses the same DOM, CSS, and render logic, with sample state supplied after each animated command. Its patch ID, file content, and terminal output are examples. Regenerate the UI copy with `python make_ui_replay.py` after editing the server UI.
- The displayed **915 Lean theorems in 30 modules** are the 397 registry and 518 kernel counts recorded in [`proofs.md`](../proofs.md). This is a theorem count, not a count of separate product guarantees or a proof of every adapter.
- The opening **3,084 Rust LoC** is the physical line count from `wc -l crates/kernel/src/*.rs` at authoring time. Lean 4 checks stated properties of extracted kernel functions; the line count is a scope cue, not a claim that every line has a separate proof.

The visual language is drawn from the o5s UI and takes inspiration from [hancore.dev](https://hancore.dev/): source text, deep black, restrained color, and deliberate motion. [`world.js`](world.js) renders an octopus from character cells, using the same code excerpt and eye style as the UI's landing page. The octopus swims between subjects; its slit pupils and a visible, thin sight line direct attention to the patches, conflict, checks, proof, Worker, Artifacts, viewer, and measured bars. A small corner mark identifies the point being discussed. All eight tentacles belong to the body. The line edit examples in the patch scene use the actual `LineEdit` fields (`path`, `base`, `start`, `end`, `content`) from `crates/platform/src/lines.rs`; the sample values and patch IDs are illustrative.
