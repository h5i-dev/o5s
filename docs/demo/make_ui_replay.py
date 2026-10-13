#!/usr/bin/env python3
"""Embed the repository's real UI with a deterministic demo data bridge."""
from pathlib import Path

HERE = Path(__file__).resolve().parent
SOURCE = HERE.parents[1] / "crates" / "server" / "src" / "index.html"
DEST = HERE / "ui-replay.html"

STYLE = """<style>
/* Film framing only. The UI markup, styles, and render logic above are copied
   verbatim from crates/server/src/index.html. */
html,body{overflow:hidden}
.frame-top{position:absolute;padding:2px 18px}
.console{padding:52px 18px 12px;max-width:none}
.console-head{margin-bottom:8px}.console-head h1{font-size:21px}
.session,.guide,.tentacle,.backdrop{display:none!important}
.toolbar{padding:6px 0}.metrics{margin-bottom:10px}
.metric{padding:7px 0}.metric strong{font-size:22px;margin-top:2px}
.grid{gap:10px;grid-template-columns:1fr 1fr}.stack{gap:10px}
.panel-head{padding:7px 10px}.panel-body,.card{padding:8px 10px}
.card p{margin:3px 0}.hash{display:none}.chips{margin-top:4px}
.activity{max-height:85px}
</style>"""

BRIDGE = """
// The film feeds example state through the genuine repository viewer renderer.
// No network request or production token is used in this deterministic replay.
let demoStep = -2;
window.addEventListener('message', event => {
  if (!event.data || event.data.type !== 'o5s-film-step') return;
  const step = event.data.step;
  if (step === demoStep) return;
  demoStep = step;
  const proposed = step >= 1, approved = step >= 2,
        checked = step >= 3, admitted = step >= 4;
  const patch = {
    id: 1, author: 1, hash: 'a71c' + '0'.repeat(60),
    intent: 'Fix parser', dependencies: [],
    approvals: approved ? [2] : [], checks_passed: checked,
    edits: [{path: 'parser.rs', content: 'parse_call()?;\\n'}], graph_edits: []
  };
  data = {
    repository: {
      channels: {main: admitted ? [1] : []},
      patches: proposed ? {'1': patch} : {},
      events: [
        ...(proposed ? [{actor: 1, action: 'propose', patch: 1}] : []),
        ...(approved ? [{actor: 2, action: 'approve', patch: 1}] : []),
        ...(checked ? [{actor: 3, action: 'check', patch: 1}] : []),
        ...(admitted ? [{actor: 2, action: 'merge', patch: 1, channel: 'main'}] : [])
      ]
    },
    view: {
      patches: admitted ? [1] : [],
      files: admitted ? {'parser.rs': '10 | parse_call()?;\\n18 | return Ok(node);\\n'} : {},
      conflicts: {}, binary: [], executable: []
    }
  };
  $('identity').textContent = 'org demo · user 1';
  const option = document.createElement('option');
  option.value = 'demo'; option.textContent = 'demo';
  $('repositories').replaceChildren(option);
  $('repositories').value = 'demo';
  render();
  if (admitted) $('files').querySelector('details')?.setAttribute('open', '');
});
"""

source = SOURCE.read_text()
assert source.endswith("</script></body></html>\n")
source = source.replace("</head>", STYLE + "</head>", 1)
source = source.replace("</script></body></html>", BRIDGE + "</script></body></html>", 1)
DEST.write_text(source)
print(f"wrote {DEST}")
