import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-sans/600.css";
import "./app.css";
import { mount } from "svelte";
import App from "./App.svelte";
import { say } from "./lib/session.svelte";

const root = document.getElementById("app")!;

/// If the interface fails to start, say why instead of showing an empty window.
function startupError(err: unknown) {
  if (root.childElementCount) return;
  const box = document.createElement("pre");
  box.style.cssText = "margin:24px;padding:16px;white-space:pre-wrap;color:#f5be8c;background:#17181b;border:1px solid #3a3d43;border-radius:6px;font:13px/1.5 monospace";
  box.textContent = `Reelforge's interface couldn't start:\n\n${err instanceof Error ? `${err.message}\n\n${err.stack ?? ""}` : String(err)}`;
  document.body.appendChild(box);
}
// Later, a failure shows in the status bar rather than going unnoticed.
const report = (err: unknown) => (root.childElementCount ? say(`Something went wrong: ${err instanceof Error ? err.message : String(err)}`) : startupError(err));
window.addEventListener("error", (e) => report(e.error ?? e.message));
window.addEventListener("unhandledrejection", (e) => report(e.reason));

try {
  mount(App, { target: root });
} catch (e) {
  startupError(e);
}
