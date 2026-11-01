// Dev helper: send obs-websocket requests to Clipper's private OBS.
// usage: node scripts/obsctl.mjs <data-dir> Request[:jsonData] ...
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
const [dataDir, ...reqs] = process.argv.slice(2);
const pw = readFileSync(`${dataDir}/obs-ws-password`, "utf8").trim();
const b64sha = (s) => createHash("sha256").update(s).digest("base64");
const ws = new WebSocket("ws://127.0.0.1:4466");
let n = 0;
ws.onmessage = (m) => {
  const msg = JSON.parse(m.data);
  if (msg.op === 0) {
    const a = msg.d.authentication;
    const auth = b64sha(b64sha(pw + a.salt) + a.challenge);
    ws.send(JSON.stringify({ op: 1, d: { rpcVersion: 1, authentication: auth, eventSubscriptions: 0 } }));
  } else if (msg.op === 2) next();
  else if (msg.op === 7) {
    console.log(msg.d.requestType, JSON.stringify(msg.d.requestStatus), JSON.stringify(msg.d.responseData ?? {}));
    next();
  }
};
function next() {
  if (n >= reqs.length) return ws.close();
  const [type, ...rest] = reqs[n].split(":");
  const data = rest.length ? JSON.parse(rest.join(":")) : {};
  ws.send(JSON.stringify({ op: 6, d: { requestType: type, requestId: String(n++), requestData: data } }));
}
