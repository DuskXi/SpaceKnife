// Temporary folders for the tests of one file (not a test file itself): each made in the system's
// temp folder with the file's prefix, and all of them removed once the file's tests have run,
// whether they passed or not.
//
//   const tmp = tempFolders("sk-release-");   // at the top of a test file
//   const dir = tmp();

import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after } from "node:test";

/** A function that makes a new temporary folder named `prefix` + 6 characters. Call it at the top
 *  of a test file: the folders go in an `after` hook of the file's tests. */
export function tempFolders(prefix) {
  const made = [];
  after(() => {
    for (const dir of made.splice(0)) rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  });
  return () => {
    const dir = mkdtempSync(join(tmpdir(), prefix));
    made.push(dir);
    return dir;
  };
}
