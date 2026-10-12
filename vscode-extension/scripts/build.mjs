// Prepares everything the extension needs at runtime:
//   --assets   copies the shared app UI (../src), LICENSE and icon into the extension
//   --sidecar  builds the Rust backend and copies it to bin/
// Without arguments it does both.
import { execFileSync } from "node:child_process";
import { copyFileSync, cpSync, existsSync, mkdirSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
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
  const sidecar = join(root, "sidecar");
  const target = process.env.SIDECAR_TARGET;
  const cargoArgs = ["build", "--release", ...(target ? ["--target", target] : [])];
  execFileSync("cargo", cargoArgs, { cwd: sidecar, stdio: "inherit" });
  const exe = process.platform === "win32" || target?.includes("windows") ? ".exe" : "";
  const name = `copilot-session-search-sidecar${exe}`;
  const built = join(sidecar, "target", ...(target ? [target] : []), "release", name);
  if (!existsSync(built)) {
    throw new Error(`Sidecar not found at ${built}`);
  }
  mkdirSync(join(root, "bin"), { recursive: true });
  copyFileSync(built, join(root, "bin", name));
  console.log(`Copied ${name} to bin/`);
}
