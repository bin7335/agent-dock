const ts = require('typescript');
const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const context = { exports: {} };
vm.runInNewContext(ts.transpileModule(fs.readFileSync('src/crewState.ts', 'utf8'), { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText, context);
const reduce = context.exports.reduceCrewEvent;
const base = { status: 'running', output: '', activity: [], permissions: [], workers: [], changes: [], planItems: [] };
const permission = id => ({ kind: 'permission_request', request_id: id, description: 'write', tool: 'command', input: '{}', suggestions: '', can_remember: false });

let task = reduce(reduce(base, permission('a')), permission('b'));
task = reduce(task, { kind: 'permission_resolved', request_id: 'a', allowed: true, auto: false });
assert.equal(task.status, 'approval');
assert.equal(task.permissions.length, 1);
task = reduce(task, { kind: 'permission_resolved', request_id: 'b', allowed: false, auto: false });
assert.equal(task.status, 'running');

task = reduce(task, { kind: 'message', text: '저장소 구조 탐색 중입니다.', delta: true });
assert.equal(task.phase, 'explore');

task = reduce(task, { kind: 'message', text: String.fromCharCode(10) + '수용 기준 및 계획:' + String.fromCharCode(10) + '- [ ] 1. 테스트 작성' + String.fromCharCode(10) + '- [x] 2. 원인 파악' + String.fromCharCode(10), delta: true });
assert.equal(task.phase, 'plan');
assert.equal(task.planItems.length, 2);
assert.equal(task.planItems[0].done, false);
assert.equal(task.planItems[1].done, true);

task = reduce(task, { kind: 'tool_use', tool: 'crew/spawn_agent', detail: '{"id":"exec-1","receiver_agents":[{"agent_nickname":"Lovelace"}],"prompt":"You are the Test Runner agent"}' });
assert.equal(task.workers.length, 1);
assert.ok(task.workers[0].name.includes('Lovelace'));
assert.equal(task.workers[0].status, 'running');

// Worker completion event
task = reduce(task, { kind: 'tool_use', tool: 'crew_done/spawn_agent', detail: '{"id":"exec-1","status":"done"}' });
assert.equal(task.workers[0].status, 'done');

task = reduce(task, { kind: 'file_change', path: 'src/lib.rs', ok: true });
assert.equal(task.changes.length, 1);
assert.equal(task.changes[0].path, 'src/lib.rs');

task = reduce(task, { kind: 'message', text: 'TDD red-green cycle 진행 중', delta: true });
assert.equal(task.phase, 'execute');

task = reduce(task, { kind: 'message', text: 'code-review 결과 보고', delta: true });
assert.equal(task.phase, 'review');

task = reduce(task, { kind: 'completed', ok: true, summary: '' });
assert.equal(reduce(task, { kind: 'process_exited', code: 0, cancelled: false }).status, 'done');
assert.equal(task.phase, 'complete');
assert.ok(task.planItems.length > 0 && task.planItems.every(p => p.done), 'all plan items must be marked done on completed ok: true');

assert.equal(reduce(base, { kind: 'process_exited', code: 0, cancelled: false }).status, 'failed');
assert.equal(reduce(base, { kind: 'process_exited', code: null, cancelled: true }).status, 'cancelled');
assert.equal(reduce(reduce(base, { kind: 'completed', ok: false, summary: 'quota' }), { kind: 'process_exited', code: 0, cancelled: false }).error, 'quota');
assert.equal(base.output, '');

// Timer & Duration tests
const timedBase = { status: 'running', startedAt: 1000, output: '', activity: [], permissions: [], workers: [], changes: [], planItems: [] };
const completedTask = reduce(timedBase, { kind: 'completed', ok: true, summary: '' });
assert.equal(completedTask.status, 'done');
assert.ok(typeof completedTask.endedAt === 'number' && completedTask.endedAt >= 1000, 'endedAt should be set on completed');

const exitedTask = reduce(timedBase, { kind: 'process_exited', code: 0, cancelled: false });
assert.ok(typeof exitedTask.endedAt === 'number' && exitedTask.endedAt >= 1000, 'endedAt should be set on process_exited');

const ongoingTask = reduce(timedBase, { kind: 'message', text: 'progress...', delta: true });
assert.equal(ongoingTask.endedAt, undefined, 'endedAt should stay unset while ongoing');

console.log('PASS crew lifecycle: phases, plan items, subagent workers, file changes, approvals, exit');
