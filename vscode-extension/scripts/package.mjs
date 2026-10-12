// Packages a platform-specific VSIX, since the bundled backend is a native binary.
// VSCE_TARGET overrides the target, e.g. VSCE_TARGET=darwin-arm64 with a matching sidecar in bin/.
import { execFileSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const target = process.env.VSCE_TARGET || `${process.platform}-${process.arch}`;
const vsce = join(root, "node_modules", "@vscode", "vsce", "vsce");
execFileSync(process.execPath, [vsce, "package", "--target", target], { cwd: root, stdio: "inherit" });
