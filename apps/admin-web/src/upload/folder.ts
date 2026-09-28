// Reading the picked folder: `<input webkitdirectory>` everywhere, or the File System Access API when
// the browser has it (which also finds empty folders). Paths are relative to the picked folder.
import type { PlannedFile } from "./types";

export interface PickedFolder {
  name: string;
  files: PlannedFile[];
  blobs: File[];
  directories: string[];
}

/** From an `<input type=file webkitdirectory>`: strips the picked folder's own name. */
export function fromFileList(list: FileList | File[]): PickedFolder {
  const files: PlannedFile[] = [];
  const blobs: File[] = [];
  let name = "";
  for (const file of Array.from(list)) {
    const rel = file.webkitRelativePath || file.name;
    const slash = rel.indexOf("/");
    if (!name) name = slash > 0 ? rel.slice(0, slash) : "";
    const path = slash > 0 ? rel.slice(slash + 1) : rel;
    files.push({ path, size: file.size, mtime_ms: file.lastModified });
    blobs.push(file);
  }
  return { name, files, blobs, directories: [] };
}

interface DirHandle {
  kind: "directory";
  name: string;
  values(): AsyncIterable<DirHandle | FileHandle>;
}
interface FileHandle {
  kind: "file";
  name: string;
  getFile(): Promise<File>;
}

export const canPickDirectory = () =>
  typeof window !== "undefined" && "showDirectoryPicker" in window;

/** File System Access API. Resolves to null when the user cancels. */
export async function pickDirectory(): Promise<PickedFolder | null> {
  const picker = (window as unknown as { showDirectoryPicker: () => Promise<DirHandle> })
    .showDirectoryPicker;
  let root: DirHandle;
  try {
    root = await picker();
  } catch {
    return null;
  }
  const out: PickedFolder = { name: root.name, files: [], blobs: [], directories: [] };
  const walk = async (dir: DirHandle, prefix: string) => {
    let empty = true;
    for await (const entry of dir.values()) {
      empty = false;
      const path = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.kind === "directory") await walk(entry, path);
      else {
        const file = await entry.getFile();
        out.files.push({ path, size: file.size, mtime_ms: file.lastModified });
        out.blobs.push(file);
      }
    }
    if (empty && prefix) out.directories.push(prefix);
  };
  await walk(root, "");
  return out;
}
