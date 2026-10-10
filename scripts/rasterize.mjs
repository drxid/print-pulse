import { Resvg } from "@resvg/resvg-js";
import { readFileSync, writeFileSync } from "node:fs";

const svg = readFileSync("reference/app-icon.svg", "utf8");
// Active tray variant: the same icon with a plain green status dot on the
// bottom-left of the frame — same size, place and colour as jig's.
const active = svg.replace(
  "<!--ACTIVE-->",
  '<circle cx="68" cy="209" r="42" fill="#34E05C"/>',
);

function render(source, size, out) {
  const r = new Resvg(source, { fitTo: { mode: "width", value: size } });
  writeFileSync(out, r.render().asPng());
  console.log("wrote", out, `${size}px`);
}

render(svg, 1024, "src-tauri/app-icon.png");
render(svg, 128, "src-tauri/icons/tray-normal.png");
render(active, 128, "src-tauri/icons/tray-active.png");
