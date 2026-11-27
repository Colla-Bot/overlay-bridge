// Copies the Windows installer that `tauri build` made into the public/ folder of CollaBot
// Overlay, checked out beside the bridge in a folder named collabot-overlay, under the name the
// dashboard's 다운로드 links to (`BRIDGE_INSTALLER_URL` in src/lib/bridge.ts there). `npm run
// build` runs it once the build has succeeded, as `postbuild`.

import { copyFileSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const readJson = (url) => JSON.parse(readFileSync(url, "utf8"));

const config = readJson(new URL("../src-tauri/tauri.conf.json", import.meta.url));
// The name Tauri gives the NSIS installer of a release build for x64, the one the site offers.
const name = `${config.productName}_${config.version}_x64-setup.exe`;
const installer = new URL(`../src-tauri/target/release/bundle/nsis/${name}`, import.meta.url);
const overlay = new URL("../../collabot-overlay/", import.meta.url);
const published = new URL("public/collabot-overlay-bridge-setup.exe", overlay);

let overlayBeside = false;
try {
  overlayBeside = readJson(new URL("package.json", overlay)).name === "collabot-overlay";
} catch {}

if (!overlayBeside) {
  console.log(
    "CollaBot Overlay, which offers the installer, is not beside the bridge in a folder named " +
      "collabot-overlay. Nothing was copied.",
  );
} else if (process.platform !== "win32") {
  console.log("Only a build on Windows makes the installer the site offers. Nothing was copied.");
} else if (!existsSync(installer)) {
  console.error(`The installer is not at ${fileURLToPath(installer)}.`);
  process.exit(1);
} else {
  mkdirSync(new URL(".", published), { recursive: true });
  copyFileSync(installer, published);
  console.log(`Copied ${name} to ${fileURLToPath(published)}.`);
}
