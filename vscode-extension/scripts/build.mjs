// Prepares everything the extension needs at runtime:
//   --assets   copies the shared app UI (../src), LICENSE and icon into the extension
//   --sidecar  builds the Rust backend and copies it to bin/ (SIDECAR_TARGET: Rust target
//              triple; SIDECAR_BINARY: copy this prebuilt backend instead of building)
// Without arguments it does both.
import { execFileSync } from "node:child_process";
import { copyFileSync, cpSync, existsSync, mkdirSync, rmSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const repo = join(root, "..");
const args = process.argv.slice(2);
const all = !args.includes("--assets") && !args.includes("--sidecar");

if (all || args.includes("--assets")) {
  const app = join(root, "media", "app");
  rmSync(app, { recursive: true, force: true });
  cpSync(join(repo, "src"), app, { recursive: true });
  copyFileSync(join(repo, "LICENSE"), join(root, "LICENSE"));
  copyFileSync(join(repo, "app-icon.png"), join(root, "icon.png"));
  console.log("Copied the app UI to media/app");
}

if (all || args.includes("--sidecar")) {
  const prebuilt = process.env.SIDECAR_BINARY;
  let built;
  if (prebuilt) {
    built = prebuilt;
  } else {
    const sidecar = join(root, "sidecar");
    const target = process.env.SIDECAR_TARGET;
    const cargoArgs = ["build", "--release", ...(target ? ["--target", target] : [])];
    execFileSync("cargo", cargoArgs, { cwd: sidecar, stdio: "inherit" });
    const exe = process.platform === "win32" || target?.includes("windows") ? ".exe" : "";
    built = join(sidecar, "target", ...(target ? [target] : []), "release", `copilot-session-manager-sidecar${exe}`);
  }
  if (!existsSync(built)) {
    throw new Error(`Sidecar not found at ${built}`);
  }
  // Only the binary of the target platform belongs in the VSIX.
  const bin = join(root, "bin");
  rmSync(bin, { recursive: true, force: true });
  mkdirSync(bin, { recursive: true });
  const name = basename(built);
  copyFileSync(built, join(bin, name));
  console.log(`Copied ${name} to bin/`);
}
