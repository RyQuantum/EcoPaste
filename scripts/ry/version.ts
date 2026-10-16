import { appendFileSync, readFileSync, writeFileSync } from "node:fs";

/**
 * RyQuantum/EcoPaste fork 的版本号：`<上游版本的下一个补丁号>-ry.<N>`，N 取 GitHub Actions 的运行序号
 * （`GITHUB_RUN_NUMBER`，每次运行自动加一）。上游 1.1.0 → 1.1.1-ry.N，比上游 1.1.0 新、比上游 1.1.1 旧。
 * 上游版本本身带预发布后缀（如 1.2.0-beta.1）时不加补丁号，直接用 1.2.0-ry.N。
 *
 * 写入 package.json、src-tauri/Cargo.toml、src-tauri/Cargo.lock，并输出 `version` / `tag` 到 GITHUB_OUTPUT。
 */
const runNumber = process.env.GITHUB_RUN_NUMBER;

if (!runNumber || !/^\d+$/.test(runNumber)) {
  throw new Error(`GITHUB_RUN_NUMBER must be a number, got "${runNumber}"`);
}

const packageJson = JSON.parse(readFileSync("package.json", "utf8"));
const upstream = String(packageJson.version);
const match = /^(\d+)\.(\d+)\.(\d+)(-.+)?$/.exec(upstream);

if (!match) {
  throw new Error(`Unexpected upstream version "${upstream}"`);
}

const [, major, minor, patch, prerelease] = match;
const base = prerelease
  ? `${major}.${minor}.${patch}`
  : `${major}.${minor}.${Number(patch) + 1}`;
const version = `${base}-ry.${runNumber}`;

packageJson.version = version;
writeFileSync("package.json", `${JSON.stringify(packageJson, null, 2)}\n`);

replaceOnce(
  "src-tauri/Cargo.toml",
  /^(version = ")[^"]+(")/m,
  `$1${version}$2`,
);
replaceOnce(
  "src-tauri/Cargo.lock",
  /(name = "EcoPaste"\r?\nversion = ")[^"]+(")/,
  `$1${version}$2`,
);

const output = process.env.GITHUB_OUTPUT;

if (output) {
  appendFileSync(output, `version=${version}\ntag=v${version}\n`);
}

process.stdout.write(`fork version ${version} (upstream ${upstream})\n`);

/**
 * 替换必须恰好命中一处，没命中说明上游改了文件格式，要响亮地失败。
 */
function replaceOnce(file: string, pattern: RegExp, replacement: string) {
  const content = readFileSync(file, "utf8");

  if (!pattern.test(content)) {
    throw new Error(`Version pattern not found in ${file}`);
  }

  writeFileSync(file, content.replace(pattern, replacement));
}
