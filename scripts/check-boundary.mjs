import { readFileSync, readdirSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { pathToFileURL } from "node:url";

const ignored = new Set(["target", "node_modules", ".git"]);
const dependencyFields = [
  "dependencies",
  "devDependencies",
  "peerDependencies",
  "optionalDependencies",
];

function scanBoundary(root) {
  root = resolve(root);
  const violations = [];
  let manifests = 0;

  function check(manifest, label, path) {
    const resolved = resolve(dirname(manifest), path);
    const fromRoot = relative(root, resolved);
    if (isAbsolute(fromRoot) || fromRoot === ".." || fromRoot.startsWith(`..${sep}`)) {
      violations.push(`boundary: ${relative(root, manifest)}: ${label} -> ${resolved}`);
    }
  }

  function walk(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        if (!ignored.has(entry.name)) walk(path);
      } else if (entry.name === "Cargo.toml") {
        manifests++;
        for (const [, doubleQuoted, singleQuoted] of readFileSync(path, "utf8").matchAll(
          /\bpath\s*=\s*(?:"([^"]+)"|'([^']+)')/g,
        )) {
          const value = doubleQuoted ?? singleQuoted;
          check(path, value, value);
        }
      } else if (entry.name === "package.json") {
        manifests++;
        const manifest = JSON.parse(readFileSync(path, "utf8"));
        for (const field of dependencyFields) {
          for (const [name, spec] of Object.entries(manifest[field] ?? {})) {
            const match = /^(?:link|file|portal):(.*)$/.exec(spec);
            if (match) check(path, name, match[1]);
          }
        }
      }
    }
  }

  walk(root);
  return { violations, manifests };
}

export function checkBoundary(root) {
  return scanBoundary(root).violations;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const { violations, manifests } = scanBoundary(process.argv[2] ?? ".");
  if (violations.length) {
    for (const violation of violations) console.error(violation);
    process.exitCode = 1;
  } else {
    console.log(`boundary ok (${manifests} manifests)`);
  }
}
