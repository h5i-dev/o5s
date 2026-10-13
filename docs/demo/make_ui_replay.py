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
  const proposedCount = Math.max(0, Math.min(3, step + 1));
  const approved = step >= 3, admitted = step >= 4;
  const base = {
    id: 42, author: 4, hash: 'b42e' + '0'.repeat(60),
    intent: 'Base snapshot', dependencies: [], approvals: [5], checks_passed: true,
    edits: [{path: 'README.md', content: 'o5s demo\\n'}], graph_edits: []
  };
  const specs = [
    ['Parser guard', 'parser.rs', 'parse_call()?;\\n'],
    ['API timeout', 'api.rs', 'timeout_ms = 500;\\n'],
    ['Parser tests', 'tests.rs', 'assert_parse_ok();\\n']
  ];
  const proposals = specs.map(([intent,path,content],i) => ({
    id: i+1, author: i+1, hash: ['a71c','b82d','c93e'][i] + '0'.repeat(60),
    intent, dependencies: [42], approvals: approved ? [4] : [],
    checks_passed: approved, edits: [{path,content}], graph_edits: []
  }));
  const patches = {'42': base};
  for(let i=0;i<proposedCount;i++) patches[String(i+1)] = proposals[i];
  data = {
    repository: {
      channels: {main: admitted ? [42,1,2,3] : [42]},
      patches,
      events: [
        ...proposals.slice(0,proposedCount).map(p => ({actor:p.author,action:'propose',patch:p.id})),
        ...(approved ? proposals.flatMap(p => [
          {actor:4,action:'approve',patch:p.id},
          {actor:5,action:'check',patch:p.id}
        ]) : []),
        ...(admitted ? proposals.map(p => ({actor:4,action:'merge',patch:p.id,channel:'main'})) : [])
      ]
    },
    view: {
      patches: admitted ? [42,1,2,3] : [42],
      files: admitted ? {
        'README.md': 'o5s demo\\n',
        'parser.rs': 'parse_call()?;\\n',
        'api.rs': 'timeout_ms = 500;\\n',
        'tests.rs': 'assert_parse_ok();\\n'
      } : {'README.md': 'o5s demo\\n'},
      conflicts: {}, binary: [], executable: []
    }
  };
  $('identity').textContent = 'org demo · 3 authors';
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
