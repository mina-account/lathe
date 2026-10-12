"use strict";
const session = new URL(location.href).searchParams.get("session");
const send = (value) => console.log("LATHE_PROBE:" + JSON.stringify(value));
window.receiveHost = (message) => {
  if (message.kind !== "Welcome" || message.session !== session || message.request !== 1) throw new Error("Invalid host response");
  document.querySelector("#status").textContent = "Typed round trip: " + session;
  send({kind: "Acknowledged", session, request: 1});
};
let sequence = 1;
const input = document.querySelector("#input");
input.addEventListener("input", () => send({kind: "Input", session, request: ++sequence, text: input.value}));
input.addEventListener("keyup", (event) => send({kind: "Event", session, request: ++sequence, event: "key_up", details: {type: "keyboard", key: event.key, key_code: event.keyCode, selection_start: input.selectionStart, selection_end: input.selectionEnd}}));
input.addEventListener("pointerdown", (event) => input.setPointerCapture(event.pointerId));
for (const [name, kind] of [["pointerdown", "pointer"], ["pointerup", "pointer_up"], ["pointermove", "drag"], ["dblclick", "double_click"], ["wheel", "wheel"]]) {
  document.addEventListener(name, (event) => {
    if (kind === "drag" && !event.buttons) return;
    send({kind: "Event", session, request: ++sequence, event: kind, details: {type: "pointer", name, shift_key: event.shiftKey, control_key: event.ctrlKey, alt_key: event.altKey, meta_key: event.metaKey, buttons: event.buttons, detail: event.detail, client_x: Math.floor(event.clientX), client_y: Math.floor(event.clientY)}});
  });
}
for (const name of ["compositionstart", "compositionupdate", "compositionend"]) {
  input.addEventListener(name, () => { document.querySelector("#composition").textContent = name; });
}
document.querySelector("#navigation").onclick = () => { location.href = "https://example.invalid/blocked"; };
document.querySelector("#popup").onclick = () => { window.open("https://example.invalid/blocked"); };
fetch("missing.bin").then((response) => {
  if (response.status !== 404) throw new Error("Unexpected resource result");
  send({kind: "Hello", session, request: 1});
});
