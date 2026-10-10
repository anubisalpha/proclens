const GB = 1024 ** 3;
const MB = 1024 ** 2;

/** "12.4 GB" / "640 MB" — always binary units, matching what Windows shows. */
export function bytes(n: number): string {
  if (n >= GB) return `${(n / GB).toFixed(1)} GB`;
  if (n >= MB) return `${Math.round(n / MB)} MB`;
  return `${Math.round(n / 1024)} KB`;
}

/** Throughput over a window, e.g. "12.3 MB/s". Zero shows as an en dash to keep tables quiet. */
export function rate(totalBytes: number, seconds: number): string {
  if (seconds <= 0 || totalBytes <= 0) return "–";
  const perSec = totalBytes / seconds;
  if (perSec >= GB) return `${(perSec / GB).toFixed(1)} GB/s`;
  if (perSec >= MB) return `${(perSec / MB).toFixed(1)} MB/s`;
  if (perSec >= 1024) return `${Math.round(perSec / 1024)} KB/s`;
  return `${Math.round(perSec)} B/s`;
}

/** "850 µs" / "1.2 ms". */
export function micros(us: number): string {
  if (us <= 0) return "–";
  if (us >= 1000) return `${(us / 1000).toFixed(1)} ms`;
  return `${Math.round(us)} µs`;
}

/** Splits "C:\dir\sub\file.txt" into directory and file name for two-line display. */
export function splitPath(path: string): { dir: string; file: string } {
  const i = path.lastIndexOf("\\");
  return i < 0 ? { dir: "", file: path } : { dir: path.slice(0, i), file: path.slice(i + 1) };
}

export function percent(part: number, whole: number): string {
  if (whole <= 0) return "0%";
  const p = (part / whole) * 100;
  if (p > 0 && p < 0.05) return "<0.1%"; // don't print a misleading "0.0%" for real but tiny activity
  return `${p < 10 ? p.toFixed(1) : Math.round(p)}%`;
}
