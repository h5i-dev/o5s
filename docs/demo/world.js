// The o5s UI draws its octopus from source-code characters. This film uses the
// same idea with an absolute-time, deterministic canvas so any frame can seek.
(() => {
  const canvas = document.getElementById('world');
  const ctx = canvas.getContext('2d');
  const guide = document.getElementById('guide');
  const gc = guide.getContext('2d');
  const CW = 8, CH = 13, COLS = 240, ROWS = 84;
  const shape = document.createElement('canvas');
  shape.width = COLS; shape.height = ROWS;
  const sc = shape.getContext('2d', { willReadFrequently: true });
  const back = document.createElement('canvas');
  back.width = 1920; back.height = 1080;
  const bc = back.getContext('2d');
  const SOURCE = `pub fn heads(rows: &Vec<Vec<usize>>, members: &Vec<usize>, covered: Vec<bool>) -> Vec<usize> {
    let mut covered = covered; let mut i = 0;
    while i < members.len() { let deps = &rows[members[i]]; let mut j = 0;
        while j < deps.len() { covered[deps[j]] = true; j += 1; } i += 1; }
theorem heads_keep_past (rows : Rows) (members : Vec Usize) : causality.heads rows members covered ⦃ hs =>
  ∀ u, (∃ x ∈ hs, Past rows u x) ↔ (∃ m ∈ members, Past rows u m) ⦄
pub fn transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome { match cmd {
    Command::Merge { name, patches } => merge(actor, s, name, patches),
theorem merge_gate : admitted p c s' → approved p s ∧ checked p s ∧ closed (deps p) c
fn validate(d: &Doc, p: &Patch) -> Result<(), Refusal> { ensure!(observed(d, p.anchors)) }
theorem extracted_view_converges : Replayed d₁ H₁ → Replayed d₂ H₂ → H₁ ~ H₂ → view d₁ = view d₂
let view = cache.sync_append_only(&repo, &channel)?; export(&root, &repo, view)
theorem isolation : actor.org ≠ repo.org → ¬ visible actor repo
`.replace(/\s+/g, ' ');
  const glyphs = Array.from({ length: COLS * ROWS }, (_, i) => SOURCE[(Math.floor(i / COLS) * 53 + (i % COLS)) % SOURCE.length]);

  // A quiet source editor in the distance, after hancore.dev's code-led feel.
  bc.fillStyle = '#050506'; bc.fillRect(0, 0, 1920, 1080);
  bc.font = '12px "IBM Plex Mono", monospace'; bc.textBaseline = 'top';
  const lines = [
    '// o5s / crates/registry/src/lib.rs',
    'pub fn transition(actor: &Principal, s: &Snapshot, cmd: &Command) -> Outcome {',
    '    match cmd {',
    '        Command::Register { name, repo } => register(actor, &s.listings, name, *repo),',
    '        Command::Create { name } => create(actor, &s.repository, name),',
    '        // other command arms',
    '    }',
    '}',
    '',
    '// o5s / proofs / DocMatRefine.lean',
    'theorem extracted_view_converges (d₁ d₂ : document.Doc) (H₁ H₂ : Hist)',
    '    -- same patches, same decoded view after materialization',
    '    := by',
    '      -- proof body in proofs/DocMatRefine.lean',
  ];
  for (let i = 0; i < lines.length; i++) {
    bc.fillStyle = i === 0 || i === 9 ? 'rgba(255,98,88,.18)' : 'rgba(235,230,219,.075)';
    bc.fillText(String(i + 1).padStart(2, '0') + '  ' + lines[i], 695, 130 + i * 23);
  }
  bc.fillStyle = 'rgba(255,98,88,.25)';
  bc.fillRect(695, 452, 1, 310);

  function octoPosition(scene, local, t) {
    const [currentX] = pointAt(scene, local);
    const previousX = scene ? pointAt(scene-1,.94)[0] : currentX;
    const transition=smooth(local/.16);
    const openingX=scene===0 ? 40+890*smooth((local-.30)/.15)+260*smooth((local-.46)/.16)-240*smooth((local-.68)/.14) : currentX;
    const targetX=previousX*(1-transition)+openingX*transition;
    return [1350 + 250 * smooth((targetX - 485) / 1000) + Math.sin(t * .36) * 24,
      265 + Math.sin(t * .68) * 18];
  }
  function form(t, scene, local) {
    const [ox, oy] = octoPosition(scene, local, t);
    const cx = ox / CW, cy = oy / CH;
    const rx = 13, ry = 7;
    const bob = Math.sin(t * .85) * .8;
    sc.fillStyle = '#000'; sc.fillRect(0, 0, COLS, ROWS);
    sc.lineCap = 'round';
    for (let i = 0; i < 8; i++) {
      const side = (i - 3.5) / 3.5;
      const sx = cx + side * rx * .55, sy = cy + ry * .45 + bob;
      const ex = cx + side * rx * 2.6;
      const ey = cy + ry * 2.25 - Math.abs(side) * 3;
      let last=[sx,sy];
      for(let k=1;k<=22;k++){
        const u=k/22;
        const wave=Math.sin(t*1.5+i*.8+u*4.4)*2.4*u*u;
        const curl=u*u*u*4.5*Math.sign(side)*(0.6+0.4*Math.sin(t*.9+i*.8));
        const x=sx+(ex-sx)*u+wave*1.4+curl;
        const y=sy+(ey-sy)*u+wave-curl*.6*u;
        sc.strokeStyle=`rgb(${Math.round(150+90*(1-u))},0,0)`;
        sc.lineWidth=Math.pow(1-u,1.4)*4.2+.35;
        sc.beginPath();sc.moveTo(...last);sc.lineTo(x,y);sc.stroke();last=[x,y];
      }
    }
    const grad = sc.createLinearGradient(cx - rx, cy - ry, cx + rx, cy + ry);
    grad.addColorStop(0, '#730000'); grad.addColorStop(.58, '#c40000'); grad.addColorStop(1, '#ff0000');
    sc.fillStyle = grad;
    sc.beginPath(); sc.ellipse(cx + rx * .17, cy - ry * .39 + bob, rx, ry * 1.02, -.35, 0, Math.PI * 2); sc.fill();
    sc.beginPath(); sc.ellipse(cx, cy + ry * .35 + bob, rx * .93, ry * .45, 0, 0, Math.PI * 2); sc.fill();
    // Match the landing page eyes: pale irises and narrow horizontal pupils.
    const blinkPhase=t%6.4;
    const lid=blinkPhase<.38 ? Math.sin(blinkPhase/.38*Math.PI) : 0;
    const [targetX,targetY]=pointAt(scene,local);
    const lookX=Math.max(-1,Math.min(1,(targetX-ox)/1920*5));
    const lookY=Math.max(-1,Math.min(1,(targetY-oy)/1080*5));
    for(const side of [-1,1]){
      const ex=cx+side*rx*.6,ey=cy+ry*.28+bob,r=2.3;
      sc.fillStyle='rgb(0,200,0)';sc.beginPath();
      sc.ellipse(ex,ey,r*1.2,r*.8*(1-lid*.9),side*.15,0,Math.PI*2);sc.fill();
      if(lid<.6){sc.fillStyle='rgb(0,0,255)';sc.fillRect(ex+lookX*r*.35-r*.8,ey+lookY*r*.2-.55,r*1.6,1.1)}
    }
    return sc.getImageData(0, 0, COLS, ROWS).data;
  }

  function drawPackets(t, scene) {
    ctx.font = '14px "IBM Plex Mono", monospace'; ctx.textBaseline = 'top';
    const hero = scene === 0;
    if (hero) {
      for (let i = 0; i < 7; i++) {
        const phase = (t * .105 + i / 7) % 1;
        const angle = i * 2.4;
        const sx = 1460 + Math.cos(angle) * 300;
        const sy = 510 + Math.sin(angle) * 320;
        const x = sx + (1390 - sx) * phase;
        const y = sy + (600 - sy) * phase;
        ctx.fillStyle = `rgba(255,120,104,${(1 - phase) * .65})`;
        ctx.fillText(`[#${String(0xa71c + i * 271).toString(16)}]`, x, y);
      }
      ctx.fillStyle = 'rgba(235,230,219,.48)';
      ctx.fillText('main  ←  immutable patch identities', 1240, 978);
    }
  }

  // The octopus gazes at the current subject. A small border mark reinforces
  // what it is looking at without drawing through any labels or body text.
  const cues = [
    [[120,103],[40,235],[920,235],[1080,315],[960,385]],
    [[1370,620],[280,480],[280,570],[280,660],[430,895],[1450,770]],
    [[850,600],[850,790]],
    [[830,605],[1260,605],[1140,690],[1140,775],[1140,858]],
    [[675,597],[675,638],[675,678],[675,718],[675,758],[1430,665]],
    [[520,505],[520,615],[965,615],[1425,665],[520,615],[965,615],[1425,580],[1425,680]],
    [[490,500],[1160,500],[1190,690]],
    [[790,750],[1490,680],[790,560],[790,650]],
    [[490,640],[950,640],[1390,640],[1635,640]],
    [[620,640],[930,640],[1260,640],[1610,640]],
    [[485,510],[485,660],[1100,470]],
  ];
  function smooth(x) { x = Math.max(0, Math.min(1, x)); return x * x * (3 - 2 * x); }
  function timedTargets(local, stops) {
    for(let i=0;i<stops.length-1;i++){
      const next=stops[i+1];
      if(local<next[0]-.035)return stops[i].slice(1);
      if(local<next[0]){
        const f=smooth((local-(next[0]-.035))/.035);
        return [stops[i][1]*(1-f)+next[1]*f,stops[i][2]*(1-f)+next[2]*f];
      }
    }
    return stops.at(-1).slice(1);
  }
  function pointAt(scene, local) {
    if(scene===0){
      return local<.16 ? [120,103] : local<.35 ? [40,235] : local<.48 ? [920,235] : local<.72 ? [1080,315] : [960,385];
    }
    if(scene===1){
      return timedTargets(local,[[0,1370,620],[.13,280,480],[.20,280,570],[.27,280,660],[.40,1370,620],[.46,430,895],[.58,270,760],[.73,1430,620],[.84,680,895],[.92,1500,770]]);
    }
    if(scene===2){
      return timedTargets(local,[[0,850,570],[.32,980,625],[.53,850,765],[.75,980,820]]);
    }
    if(scene===3){
      const path=[[0,830,605],[.18,1265,605],[.30,830,605],[.40,1147,690],[.50,830,605],[.58,1147,775],[.66,830,605],[.75,1147,858],[.86,1545,690],[1,1430,858]];
      const i=Math.max(0,path.findIndex((p,j)=>j<path.length-1&&local>=p[0]&&local<=path[j+1][0]));
      const a=path[i],b=path[i+1],f=smooth((local-a[0])/(b[0]-a[0]));
      return [a[1]+(b[1]-a[1])*f,a[2]+(b[2]-a[2])*f];
    }
    if(scene===4){
      return local<.18 ? [675,597] : local<.32 ? [675,638] : local<.48 ? [675,678] : local<.6 ? [675,718] : local<.72 ? [675,758] : [1430,665];
    }
    if(scene===5){
      const path=[[0,520,505],[.16,520,615],[.31,965,615],[.45,1425,665],[.49,520,615],[.65,965,615],[.78,1425,580],[1,1425,680]];
      const i=Math.max(0,path.findIndex((p,j)=>j<path.length-1&&local>=p[0]&&local<=path[j+1][0]));
      const a=path[i],b=path[i+1],f=smooth((local-a[0])/(b[0]-a[0]));
      return [a[1]+(b[1]-a[1])*f,a[2]+(b[2]-a[2])*f];
    }
    if(scene===6){
      return timedTargets(local,[[0,520,620],[.20,1140,720],[.31,1250,625],[.35,520,470],[.44,850,470],[.53,1190,470],[.62,1550,470]]);
    }
    if(scene===7){
      return timedTargets(local,[[0,790,750],[.27,1490,675],[.66,790,555],[.80,790,650],[.91,790,840]]);
    }
    if(scene===8){
      // Each change leads the corresponding noun in the Cloudflare voice-over.
      return timedTargets(local,[[0,650,640],[.11,1050,640],[.25,1390,640],[.32,1635,640],[.43,650,640],[.58,1050,640],[.70,1635,640]]);
    }
    const targets = cues[scene];
    const u = Math.max(0, Math.min(targets.length - 1, (local - .12) / .75 * (targets.length - 1)));
    const i = Math.min(targets.length - 2, Math.floor(u));
    const f = smooth((u - i - .65) / .35);
    return [targets[i][0] * (1 - f) + targets[i + 1][0] * f,
      targets[i][1] * (1 - f) + targets[i + 1][1] * f];
  }
  function drawSight(t,scene,local){
    const [ox,oy]=octoPosition(scene,local,t);
    const [tx,ty]=pointAt(scene,local);
    const x=ox-13*.6*CW,y=oy+7*.28*CH;
    const fade=smooth(local/.1)*(1-smooth((local-.96)/.04));
    const grad=gc.createLinearGradient(x,y,tx,ty);
    grad.addColorStop(0,'rgba(255,186,164,.9)');
    grad.addColorStop(1,'rgba(255,98,88,.65)');
    gc.save();gc.globalAlpha=fade;gc.lineWidth=1.35;gc.setLineDash([3,6]);
    gc.strokeStyle=grad;gc.beginPath();gc.moveTo(x,y);
    if(scene===0 && ty<150){
      gc.bezierCurveTo(x-80,y-95,1010,105,700,105);
      gc.bezierCurveTo(390,105,120,105,tx,ty);
    } else if(scene===0 && tx<100){
      // Follow the open gutter below the headline to the red "1,000 agents".
      // A straight line would run across both lines of copy.
      gc.bezierCurveTo(x-80,y+90,1010,405,700,405);
      gc.bezierCurveTo(390,405,35,405,35,360);
      gc.lineTo(tx,ty);
    } else gc.lineTo(tx,ty);
    gc.stroke();gc.restore();
  }
  function drawFocus(t, scene, local) {
    gc.clearRect(0,0,1920,1080);
    drawSight(t,scene,local);
    const [x,y]=pointAt(scene,local);
    const a=smooth(local/.08)*(1-smooth((local-.95)/.05));
    const r=13+Math.sin(t*3)*1.5;
    gc.save();gc.globalAlpha=.8*a;gc.strokeStyle='#ff6258';gc.lineWidth=1.5;
    for(const [sx,sy] of [[-1,-1],[1,-1],[-1,1],[1,1]]){
      gc.beginPath();gc.moveTo(x+sx*r,y+sy*(r-5));gc.lineTo(x+sx*r,y+sy*r);
      gc.lineTo(x+sx*(r-5),y+sy*r);gc.stroke();
    }
    gc.restore();
  }

  window.drawWorld = (ms, scene, local = 0) => {
    const t = ms / 1000;
    ctx.drawImage(back, 0, 0);
    const px = form(t, scene, local);
    ctx.save();
    ctx.globalAlpha = scene === 0 || scene === 10 ? .2 : .13;
    ctx.filter = 'blur(28px)';
    ctx.drawImage(shape, 0, 0, 1920, 1080);
    ctx.restore();
    const shift = Math.floor(t * 2);
    ctx.font = '13px "IBM Plex Mono", monospace'; ctx.textBaseline = 'top';
    for (let y = 0; y < ROWS; y++) for (let x = 0; x < COLS; x++) {
      const n = y * COLS + x, r = px[n * 4], g = px[n * 4 + 1], b = px[n * 4 + 2];
      if (r < 25 && g < 90 && b < 100) continue;
      const char = glyphs[(n + (y % 3 === 0 ? shift : 0)) % glyphs.length];
      if (b > 100) continue;
      if (g > 90) ctx.fillStyle = '#e1d0b4';
      else { const k = r / 255; ctx.fillStyle = `rgba(${Math.round(142 + 113 * k)},${Math.round(22 + 70 * k)},${Math.round(18 + 60 * k)},${.5 + k * .5})`; }
      ctx.fillText(char === ' ' ? '·' : char, x * CW, y * CH);
    }
    drawPackets(t, scene);
    drawFocus(t, scene, local);
  };
})();
