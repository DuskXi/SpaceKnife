// Browser mode's path dialog (shim.ts): a browser can't show the folders of the computer the files
// are on, so a file or folder there is typed instead of picked. PathDialog.tsx draws the request.
// Kept apart from the app store: the shim installs before the store exists.

export interface PathRequest {
  title: string;
  /** what to type, under the title (which computer, which kind of file) */
  hint: string;
  /** the text to start from: a folder there, or the suggested file name */
  value: string;
}

interface Pending extends PathRequest {
  answer: (value: string | null) => void;
}

let pending: Pending | null = null;
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((cb) => cb());

/** Ask for a path; null when cancelled or left empty. A new question cancels the one shown. */
export function askPath(request: PathRequest): Promise<string | null> {
  pending?.answer(null);
  return new Promise((done) => {
    pending = { ...request, answer: (value) => done(value?.trim() || null) };
    emit();
  });
}

/** The dialog's answer (null: cancelled). */
export function answerPath(value: string | null): void {
  const p = pending;
  pending = null;
  emit();
  p?.answer(value);
}

export const shownPath = (): PathRequest | null => pending;

export function onPathChange(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}
