const { spawn } = require('child_process');
const p = spawn('codex', ['app-server'], { shell: true, cwd: process.cwd() });
let buf = '';
const send = o => p.stdin.write(JSON.stringify(o) + '\n');
p.stdout.on('data', d => {
  buf += d; let i;
  while ((i = buf.indexOf('\n')) >= 0) {
    const line = buf.slice(0, i); buf = buf.slice(i + 1);
    let m; try { m = JSON.parse(line); } catch { continue; }
    if (m.id === 1) { send({ method: 'initialized' }); send({ id: 2, method: 'skills/list', params: { cwds: [process.cwd()] } }); }
    if (m.id === 2) {
      const entries = (m.result && m.result.data) || [];
      for (const e of entries) {
        const names = (e.skills || []).map(s => s.name);
        console.log('count', names.length);
        console.log('all:', names.join(', '));
        console.log('matt:', names.filter(n => /mattpocock|tdd|code-review|grilling|diagnosing|prototype|codebase-design/.test(n)).join(', '));
        for (const er of (e.errors || [])) console.log('ERR', er.path, er.message);
      }
      if (!entries.length) console.log(JSON.stringify(m).slice(0, 1500));
      p.kill(); process.exit(0);
    }
  }
});
send({ id: 1, method: 'initialize', params: { clientInfo: { name: 'agent-dock-probe', version: '0.0.1' } } });
setTimeout(() => { console.log('timeout'); p.kill(); process.exit(1); }, 25000);

