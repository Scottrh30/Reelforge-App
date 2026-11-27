// 3D Model Properties: the dialog that sets a model up as it's added (or a model
// layer later). One at a time.
import { api, type TitleSpec } from "./api";

export type PartLook = {
  part: string;
  diffuse_map?: string | null;
  specular: [number, number, number];
  shininess: number;
  emissive: [number, number, number];
  opacity: number;
};
export type ModelSource = {
  path: string;
  z_up?: boolean;
  own_colours?: boolean;
  rev?: number;
  part_colours?: [string, [number, number, number]][];
  texture_dirs?: string[];
  materials?: PartLook[];
  hidden?: string[];
};

export type ModelDialog =
  | { mode: "add"; source: ModelSource; name: string }
  | { mode: "edit"; id: number; spec: TitleSpec; source: ModelSource; name: string };

export const modelDialog = $state<{ open: ModelDialog | null }>({ open: null });

const stem = (p: string) => (p.split(/[\\/]/).pop() ?? p).replace(/\.[^.]+$/, "");

/// Pick a model file and set it up before it's added (Timeline → Add 3D model…).
export async function importModel() {
  const path = await api.pickModelFile();
  if (!path) return;
  modelDialog.open = { mode: "add", source: { path, own_colours: true }, name: stem(path) };
}

/// Set up a model layer that's already there.
export function editModel(id: number, spec: TitleSpec) {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const model = (spec as any).model as ModelSource | undefined;
  if (!model) return;
  modelDialog.open = { mode: "edit", id, spec: structuredClone($state.snapshot(spec)) as TitleSpec, source: structuredClone($state.snapshot(model)) as ModelSource, name: spec.text };
}
