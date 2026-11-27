// Writes THIRD-PARTY-LICENSES.txt: the software by others that the bridge is built from, Overlayed,
// Microsoft's WebView2 loader and the Rust crates, with their licenses, which cargo-about collects
// for the crates as src-tauri/about.toml says. `tauri build` runs it first, as `npm run licenses`,
// and every bundle installs the file beside the program.

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const bridge = fileURLToPath(new URL("..", import.meta.url));
const output = new URL("../THIRD-PARTY-LICENSES.txt", import.meta.url);

const rule = "-".repeat(80);

let about;
try {
  about = execFileSync(
    "cargo",
    ["about", "generate", "--format", "json", "--fail", "-m", "src-tauri/Cargo.toml"],
    {
      cwd: bridge,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
      stdio: ["ignore", "pipe", "inherit"],
    },
  );
} catch (error) {
  console.error(
    "Could not list the crates' licenses. cargo-about is needed: " +
      "cargo install cargo-about --locked --features cli",
  );
  throw error;
}
const { licenses: found } = JSON.parse(about);

// cargo-about also takes license headers from source files, such as a C header a crate vendors.
// Such a text is left out for a crate that has a license file too, and replaced by the license's
// standard text, which cargo-about gives crates packaged without a license file, for one that
// has not.
const isLicenseFile = (path) =>
  /^(licen[cs]e|copying|copyright|notice|unlicense)/i.test(path.split(/[\\/]/).pop());
const fromSource = (l) => l.source_path && !isLicenseFile(l.source_path);
const standard = new Map(found.filter((l) => !l.source_path).map((l) => [l.id, l]));
const listed = new Set(
  found.filter((l) => !fromSource(l)).flatMap((l) => l.used_by.map((u) => u.crate.id)),
);
for (const l of found.filter(fromSource)) {
  const unlisted = l.used_by.filter((u) => !listed.has(u.crate.id));
  if (unlisted.length > 0 && standard.has(l.id)) {
    standard.get(l.id).used_by.push(...unlisted);
    l.used_by = [];
  } else {
    l.used_by = unlisted;
  }
}
const licenses = found.filter((l) => l.used_by.length > 0);
for (const l of licenses) l.used_by.sort((a, b) => a.crate.name.localeCompare(b.crate.name));

const intro = `CollaBot Overlay Bridge is built from the following software by others.

The part of the program that talks to Discord is a Rust port of the RPC manager
of Overlayed (https://github.com/overlayeddev/overlayed), which is licensed
under the GNU Affero General Public License 3.0, as the program is.

The software below is built into the program, each under the license that
follows its name. A crate offered under a choice of licenses is used under the
one given.
`;

// webview2-com-sys, MIT-licensed, links Microsoft's WebView2 loader into the program on Windows,
// which is under a license of its own that cargo-about cannot see. webview2-loader-license.txt is
// LICENSE.txt of the Microsoft.Web.WebView2 package on NuGet, which the crate takes the loader from.
const usesLoader = found.some((l) => l.used_by.some((u) => u.crate.name === "webview2-com-sys"));
const loaderLicense = readFileSync(new URL("webview2-loader-license.txt", import.meta.url), "utf8");
const loader = `${rule}
BSD 3-Clause License, used by:

- WebView2 loader, which webview2-com-sys builds into the program on Windows
  (https://www.nuget.org/packages/Microsoft.Web.WebView2)

${loaderLicense.replace(/\r\n?/g, "\n").trim()}
`;

const sections = licenses.map(({ name, text, used_by }) => {
  const crates = used_by
    .map(({ crate }) => {
      const where = crate.repository ?? crate.homepage;
      return `- ${crate.name} ${crate.version}${where ? ` (${where})` : ""}`;
    })
    .join("\n");
  const lines = text.replace(/\r\n?/g, "\n").trim();
  return `${rule}\n${name}, used by:\n\n${crates}\n\n${lines}\n`;
});

if (usesLoader) sections.unshift(loader);
writeFileSync(output, `${intro}\n${sections.join("\n")}`);
console.log(
  `THIRD-PARTY-LICENSES.txt lists ${licenses.reduce((n, l) => n + l.used_by.length, 0)} crates' licenses`,
);
