import fs from "node:fs";
import path from "node:path";

if (process.platform === "darwin") {
  const helper = path.join("node_modules", "node-pty", "prebuilds", `darwin-${process.arch}`, "spawn-helper");
  if (fs.existsSync(helper)) fs.chmodSync(helper, 0o755);
}
