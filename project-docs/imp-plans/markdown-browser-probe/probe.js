"use strict";
const send = (message) => window.ipc.postMessage(JSON.stringify(message));
const session = window.probeSession;
window.receiveHost = (message) => {
  if (message.kind !== "Welcome" || message.session !== session || message.request !== 1) {
    throw new Error("Invalid native response");
  }
  document.querySelector("#status").textContent = `Typed round trip: ${session}`;
  send({kind: "Acknowledged", session, request: message.request});
};
const input = document.querySelector("#input");
let sequence = 1;
input.addEventListener("input", () => send({kind: "Input", session, request: ++sequence, text: input.value}));
for (const event of ["compositionstart", "compositionupdate", "compositionend"]) {
  input.addEventListener(event, () => {
    document.querySelector("#composition").textContent = event;
  });
}
document.querySelector("#navigation").onclick = () => { location.href = "https://example.invalid/blocked"; };
document.querySelector("#popup").onclick = () => { window.open("https://example.invalid/blocked"); };
fetch("missing.bin").then((response) => {
  if (response.status !== 404) throw new Error("Unpackaged resource was served");
  send({kind: "Hello", session, request: 1});
});
