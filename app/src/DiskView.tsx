import { useState } from "react";
import type { ProcessActivity, SystemProfile } from "./api";
import { CheckWarning } from "./CheckWarning";
import type { ActivityWindow } from "./activityWindow";
import { bytes, micros, percent, rate, splitPath } from "./format";

const SHOWN = 15;

type SortBy = "disk" | "file";
const UNKNOWN_PID = 4294967295;

/** Checks this view depends on: if any failed on this PC, its numbers may be wrong. */
const DISK_CHECKS = ["etw_kernel_session", "etw_disk_io", "etw_file_io", "handle_table_names"];

export function DiskView({ win, profile }: { win: ActivityWindow | null; profile: SystemProfile | null }) {
  const [sortBy, setSortBy] = useState<SortBy>("disk");
  if (!win) {
    return (
      <>
        <CheckWarning profile={profile} ids={DISK_CHECKS} />
        <p className="dim" role="status">Starting the trace… the first reading takes a couple of seconds.</p>
      </>
    );
  }
  const s = win.seconds;
  const totalRead = win.processes.reduce((n, p) => n + p.disk_read_bytes, 0);
  const totalWrite = win.processes.reduce((n, p) => n + p.disk_write_bytes, 0);

  const disk = (p: ProcessActivity) => p.disk_read_bytes + p.disk_write_bytes;
  const file = (p: ProcessActivity) => p.file_read_bytes + p.file_write_bytes;
  const [primary, secondary] = sortBy === "disk" ? [disk, file] : [file, disk];
  const procs = [...win.processes]
    .sort((a, b) => primary(b) - primary(a) || secondary(b) - secondary(a))
    .slice(0, SHOWN);

  const h = win.health;
  const unresolvedShare = h.file_bytes_total > 0 ? h.unresolved_file_bytes / h.file_bytes_total : 0;

  return (
    <>
      <CheckWarning profile={profile} ids={DISK_CHECKS} />
      <section aria-labelledby="disk-h" className="card">
        <h2 id="disk-h">Disk activity</h2>
        <p className="lede">
          Average over the last {Math.round(s)} seconds. The whole PC is reading{" "}
          <strong>{rate(totalRead, s)}</strong> from and writing <strong>{rate(totalWrite, s)}</strong> to its
          disks.
        </p>

        {h.events_lost > 0 && (
          <p className="banner error" role="alert">
            Windows dropped {h.events_lost.toLocaleString()} trace events, so the figures below may be too low.
          </p>
        )}

        <div className="sortbar" role="group" aria-label="Sort processes by">
          Sort by
          {(["disk", "file"] as const).map((k) => (
            <button
              key={k}
              className={sortBy === k ? "chip on" : "chip"}
              aria-pressed={sortBy === k}
              onClick={() => setSortBy(k)}
            >
              {k === "disk" ? "Physical disk" : "File requests"}
            </button>
          ))}
        </div>

        <table className="plain procs">
          <thead>
            <tr>
              <th scope="col">Process</th>
              <th scope="col" className="num">Disk read</th>
              <th scope="col" className="num">Disk write</th>
              <th scope="col" className="num">Disk delay</th>
              <th scope="col" className="num">File read</th>
              <th scope="col" className="num">File write</th>
            </tr>
          </thead>
          <tbody>
            {procs.map((p) => (
              <tr key={p.pid}>
                <th scope="row">
                  {p.name} {p.pid !== UNKNOWN_PID && <span className="dim pid">{p.pid}</span>}
                </th>
                <td className="num">{rate(p.disk_read_bytes, s)}</td>
                <td className="num">{rate(p.disk_write_bytes, s)}</td>
                <td className="num">{p.disk_latency_us === null ? "–" : micros(p.disk_latency_us)}</td>
                <td className="num dim">{rate(p.file_read_bytes, s)}</td>
                <td className="num dim">{rate(p.file_write_bytes, s)}</td>
              </tr>
            ))}
          </tbody>
        </table>
        <p className="note">
          <strong>Disk</strong> columns are what actually reached the physical disk. <strong>File</strong> columns
          are what each program asked its files for, before Windows' cache: a program can read a lot of file
          data and touch the disk very little. "Disk delay" is the average time the disk took per request
          (approximate).
        </p>
      </section>

      <section aria-labelledby="files-h" className="card">
        <h2 id="files-h">Busiest files</h2>
        <p className="lede">Most data read or written in the last {Math.round(s)} seconds, by any program.</p>
        <table className="plain files">
          <tbody>
            {win.files.slice(0, SHOWN).map((f) => {
              const { dir, file } = splitPath(f.path);
              return (
                <tr key={f.path}>
                  <th scope="row" title={f.path}>
                    <span className="file">{file || f.path}</span>
                    {dir && <span className="dir dim">{dir}</span>}
                  </th>
                  <td className="num">{rate(f.bytes, s)}</td>
                  <td className="num dim">{bytes(f.bytes)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {unresolvedShare > 0.05 && (
          <p className="note">
            {percent(h.unresolved_file_bytes, h.file_bytes_total)} of file traffic couldn't be matched to a file
            name (the file was opened before tracing began, or closed before it could be identified).
          </p>
        )}
      </section>
    </>
  );
}
