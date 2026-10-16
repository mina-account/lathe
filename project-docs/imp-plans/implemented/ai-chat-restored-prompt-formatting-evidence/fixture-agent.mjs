import { createInterface } from 'node:readline';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname } from 'node:path';

const statePath = process.argv[2];
if (!statePath) throw new Error('Pass a task-owned state JSON path');
mkdirSync(dirname(statePath), { recursive: true });
let state;
try {
  state = JSON.parse(readFileSync(statePath, 'utf8'));
} catch (error) {
  if (error.code !== 'ENOENT') throw error;
  state = { sessions: {}, loads: 0 };
}

function save() {
  writeFileSync(statePath, JSON.stringify(state, null, 2));
}
function send(value) {
  process.stdout.write(`${JSON.stringify({ jsonrpc: '2.0', ...value })}\n`);
}
function update(sessionId, sessionUpdate, text) {
  send({ method: 'session/update', params: {
    sessionId, update: { sessionUpdate, content: { type: 'text', text } },
  } });
}
function flatten(block) {
  if (block.type === 'text') return block.text;
  if (block.type === 'resource_link') return `[@${block.name}](${block.uri})`;
  if (block.type === 'resource' && 'text' in block.resource) {
    const resource = block.resource;
    const name = resource.uri.split('/').at(-1);
    return `[@${name}](${resource.uri})\n<context ref="${resource.uri}">\n${resource.text}\n</context>`;
  }
  throw new Error(`Unsupported fixture block: ${block.type}`);
}
function replay(sessionId) {
  const session = state.sessions[sessionId];
  if (!session) throw new Error(`Unknown fixture session ${sessionId}`);
  for (const prompt of session.prompts) {
    update(sessionId, 'user_message_chunk', prompt);
    update(sessionId, 'agent_thought_chunk', 'Inspecting the captured attachment.');
    update(sessionId, 'agent_message_chunk', 'A concise quiz is ready.');
  }
}

for await (const line of createInterface({ input: process.stdin })) {
  if (!line.trim()) continue;
  const request = JSON.parse(line);
  if (request.id === undefined) continue;
  try {
    let result;
    switch (request.method) {
      case 'initialize':
        result = {
          protocolVersion: request.params.protocolVersion,
          agentInfo: { name: 'prompt-restoration-fixture', version: '1.0.0' },
          agentCapabilities: {
            loadSession: true,
            promptCapabilities: { embeddedContext: true },
            sessionCapabilities: { list: {} },
          },
          authMethods: [],
        };
        break;
      case 'session/new': {
        const sessionId = `prompt-restore-${Object.keys(state.sessions).length + 1}`;
        state.sessions[sessionId] = { cwd: request.params.cwd, prompts: [] };
        save();
        result = { sessionId };
        break;
      }
      case 'session/load':
        state.loads += 1;
        save();
        replay(request.params.sessionId);
        result = {};
        break;
      case 'session/list':
        result = { sessions: Object.entries(state.sessions).map(([sessionId, session]) => ({
          sessionId, cwd: session.cwd, title: 'Prompt restoration fixture',
          updatedAt: '2026-10-14T07:00:00Z',
        })) };
        break;
      case 'session/prompt': {
        const sessionId = request.params.sessionId;
        state.sessions[sessionId].prompts.push(request.params.prompt.map(flatten).join(''));
        save();
        update(sessionId, 'agent_thought_chunk', 'Inspecting the captured attachment.');
        update(sessionId, 'agent_message_chunk', 'A concise quiz is ready.');
        result = { stopReason: 'end_turn' };
        break;
      }
      default:
        throw new Error(`Unsupported fixture request ${request.method}`);
    }
    send({ id: request.id, result });
  } catch (error) {
    send({ id: request.id, error: { code: -32603, message: error.message } });
  }
}
