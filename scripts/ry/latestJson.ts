import { createHash, createPublicKey, verify } from "node:crypto";
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";

/**
 * 为 fork 的 Releases 生成 Tauri updater 用的 latest.json，生成前先用 tauri.conf.json 里的公钥
 * 校验安装包签名：公钥和签名密钥对不上时，已安装的 EcoPaste 会拒绝这次升级，必须在发布前就失败。
 *
 * 用法：tsx scripts/ry/latestJson.ts <版本号> <NSIS 产物目录> <owner/repo> <release notes 文件> <输出路径>
 */
const [version, bundleDir, repo, notesFile, outFile] = process.argv.slice(2);

if (!version || !bundleDir || !repo || !notesFile || !outFile) {
  throw new Error(
    "usage: latestJson.ts <version> <bundleDir> <owner/repo> <notesFile> <outFile>",
  );
}

const installers = readdirSync(bundleDir).filter((name) => {
  return name.endsWith("-setup.exe") && name.includes(version);
});

if (installers.length !== 1) {
  throw new Error(
    `Expected exactly one ${version} NSIS installer in ${bundleDir}, found: ${installers.join(", ") || "none"}`,
  );
}

const installer = installers[0];
const installerPath = join(bundleDir, installer);
const signature = readFileSync(`${installerPath}.sig`, "utf8").trim();
const config = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));

verifyMinisign(
  readFileSync(installerPath),
  signature,
  config.plugins.updater.pubkey,
);

const tag = `v${version}`;
const url = `https://github.com/${repo}/releases/download/${tag}/${encodeURIComponent(basename(installer))}`;
const platform = { signature, url };
const latest = {
  notes: readFileSync(notesFile, "utf8").trim(),
  platforms: {
    "windows-x86_64": platform,
    "windows-x86_64-nsis": platform,
  },
  pub_date: new Date().toISOString(),
  version,
};

writeFileSync(outFile, `${JSON.stringify(latest, null, 2)}\n`);
process.stdout.write(`wrote ${outFile} for ${installer}\n`);

/**
 * 按 minisign 格式校验签名（Tauri updater 用的就是它）：文件签名 + 全局签名都要对，
 * 密钥 ID 要和公钥一致。签名算法 `ED` 表示先对文件做 BLAKE2b-512 再签。
 */
function verifyMinisign(
  file: Buffer,
  signatureBase64: string,
  pubkeyBase64: string,
) {
  const pubkeyLines = decodeLines(pubkeyBase64);
  const pubkey = Buffer.from(pubkeyLines[1], "base64");
  const keyId = pubkey.subarray(2, 10);
  const key = createPublicKey({
    format: "jwk",
    key: {
      crv: "Ed25519",
      kty: "OKP",
      x: pubkey.subarray(10, 42).toString("base64url"),
    },
  });

  const signatureLines = decodeLines(signatureBase64);
  const sig = Buffer.from(signatureLines[1], "base64");
  const trustedComment = signatureLines[2].replace(/^trusted comment: /, "");
  const globalSig = Buffer.from(signatureLines[3], "base64");
  const algorithm = sig.subarray(0, 2).toString("latin1");

  if (!sig.subarray(2, 10).equals(keyId)) {
    throw new Error(
      "Installer was signed with a different key than tauri.conf.json plugins.updater.pubkey",
    );
  }

  const message =
    algorithm === "ED" ? createHash("blake2b512").update(file).digest() : file;

  if (!verify(null, message, key, sig.subarray(10, 74))) {
    throw new Error(
      "Installer signature does not verify against the updater pubkey",
    );
  }

  const globalMessage = Buffer.concat([
    sig.subarray(10, 74),
    Buffer.from(trustedComment, "utf8"),
  ]);

  if (!verify(null, globalMessage, key, globalSig)) {
    throw new Error("Installer signature trusted comment does not verify");
  }
}

function decodeLines(base64: string) {
  return Buffer.from(base64, "base64")
    .toString("utf8")
    .split(/\r?\n/)
    .filter((line) => {
      return line.length > 0;
    });
}
