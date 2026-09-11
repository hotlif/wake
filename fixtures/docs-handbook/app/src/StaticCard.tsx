import { css } from "@crab-dev/css";
const unit = 4;
const gap = `${unit * 2}px`;
const box = css`padding: ${gap};`;
export function StaticCard() { return <div className={box}>固定间距</div>; }
