import fs from "node:fs";
import { execSync } from "node:child_process";

function setOutput(key, value) {
  const outputPath = process.env.GITHUB_OUTPUT;
  const line = `${key}=${value}\n`;
  if (outputPath) {
    fs.appendFileSync(outputPath, line);
  } else {
    process.stdout.write(line);
  }
}

const subject = execSync("git log -1 --pretty=%s", { encoding: "utf8" }).trim();
const skip = subject.startsWith("chore: release v");
setOutput("skip", skip);
