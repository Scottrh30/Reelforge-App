// Plane Properties: which plane's dialog is open (one at a time).
export const planeDialog = $state<{ id: number | null }>({ id: null });

export function openPlaneProps(id: number) {
  planeDialog.id = id;
}
