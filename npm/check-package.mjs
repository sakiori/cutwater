// The prepack check of cutwater's npm packages, which run it with npm's
// packing: the package directory holds LICENSE, and a platform package
// `cutwater`, an executable for its os and cpu, with its third-party
// notices. The pipeline stages them from the release archives
// (.gitlab/scripts/stage-npm.sh); without the check, npm would pack a
// platform package without them.
//
//   node ../check-package.mjs [<os> <cpu>]
import { accessSync, constants, readFileSync } from "node:fs";

// The ELF machine and the Mach-O CPU type of each npm cpu.
const ELF_MACHINE = { x64: 0x3e, arm64: 0xb7 };
const MACHO_CPU = { x64: 0x01000007, arm64: 0x0100000c };

function fail(message) {
  console.error(`${process.cwd()}: ${message}; .gitlab/scripts/stage-npm.sh stages it from the release archives`);
  process.exit(1);
}

try {
  accessSync("LICENSE", constants.R_OK);
} catch {
  fail("LICENSE is missing");
}
const [os, cpu] = process.argv.slice(2);
if (os !== undefined) {
  for (const notices of ["THIRD-PARTY-NOTICES.txt", "RUST-LIBRARY-COPYRIGHT.html"]) {
    try {
      accessSync(notices, constants.R_OK);
    } catch {
      fail(`${notices} is missing`);
    }
  }
  let bytes;
  try {
    accessSync("cutwater", constants.X_OK);
    bytes = readFileSync("cutwater");
  } catch {
    fail("cutwater is missing or not executable");
  }
  const elf = bytes.length > 20 && bytes.readUInt32BE(0) === 0x7f454c46;
  const macho = bytes.length > 8 && bytes.readUInt32LE(0) === 0xfeedfacf;
  const fits =
    os === "linux"
      ? elf && bytes[4] === 2 && bytes.readUInt16LE(18) === ELF_MACHINE[cpu]
      : os === "darwin" && macho && bytes.readUInt32LE(4) === MACHO_CPU[cpu];
  if (!fits) fail(`cutwater is not an executable for ${os} on ${cpu}`);
}
