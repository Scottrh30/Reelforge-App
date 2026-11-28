// Reelforge's own colour picker: a pop-up with OK and Cancel (the system's picker
// only takes a colour when you click away from it). One at a time.

export type ColorPick = {
  /// The colour it starts at (#rrggbb).
  start: string;
  /// Where it opens (the window's pixels): beside what was clicked.
  x: number;
  y: number;
  title: string;
  /// Called with the colour on OK (not on Cancel).
  done: (hex: string) => void;
};

/// Settings → Interface → the system's colour picker instead (set from the prefs).
export const useNative = { on: false };

export const colorPick = $state<{ open: ColorPick | null }>({ open: null });

/// Open the picker at `start`; `done` gets the colour if it's accepted with OK.
/// `from` (an element or event) places it; without, it opens in the middle.
export function pickColor(start: string, done: (hex: string) => void, from?: Element | MouseEvent | null, title = "Colour") {
  let x = window.innerWidth / 2 - 130;
  let y = window.innerHeight / 2 - 160;
  if (from instanceof MouseEvent) {
    x = from.clientX;
    y = from.clientY;
  } else if (from instanceof Element) {
    const r = from.getBoundingClientRect();
    x = r.left;
    y = r.bottom + 4;
  }
  const hex = /^#[0-9a-f]{6}$/i.test(start) ? start.toLowerCase() : "#808080";
  // Settings → Interface → use the system's colour picker.
  if (useNative.on) {
    document.querySelectorAll(".rf-native-color").forEach((e) => e.remove());
    const input = document.createElement("input");
    input.className = "rf-native-color";
    input.type = "color";
    input.value = hex;
    input.style.cssText = `position:fixed;left:${x}px;top:${y}px;width:1px;height:1px;opacity:0;pointer-events:none`;
    document.body.appendChild(input);
    input.addEventListener("change", () => {
      done(input.value.toLowerCase());
      input.remove();
    });
    input.click();
    return;
  }
  colorPick.open = { start: hex, x, y, title, done };
}
