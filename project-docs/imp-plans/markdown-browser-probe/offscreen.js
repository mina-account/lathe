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
for (const [name, event] of [["pointerdown", "pointer"], ["wheel", "wheel"], ["keyup", "key_up"]]) {
  input.addEventListener(name, () => send({kind: "Event", session, request: ++sequence, event}));
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
