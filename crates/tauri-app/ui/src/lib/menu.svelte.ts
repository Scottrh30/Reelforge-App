// One context menu at a time, opened at the pointer.

export interface MenuItem {
  label?: string;
  hint?: string;
  disabled?: boolean;
  checked?: boolean;
  run?: () => void;
  /// A dividing line instead of an item.
  separator?: boolean;
  /// A heading (not clickable).
  heading?: boolean;
}

export const menu = $state({ open: false, x: 0, y: 0, items: [] as MenuItem[] });

export function openMenu(e: MouseEvent, items: MenuItem[]) {
  e.preventDefault();
  e.stopPropagation();
  menu.items = items;
  menu.x = e.clientX;
  menu.y = e.clientY;
  menu.open = true;
}

export function closeMenu() {
  menu.open = false;
}
