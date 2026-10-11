# Anomaly detection: can we, and how

Status: **scoping only, nothing built.** Written 2026-10-11, after the Overview page and the optional
activity log were built (they are what this would sit on).

## Short answer

Yes, for a modest and explainable version: plain rules plus a rolling "what is normal for this PC
right now" baseline on a handful of numbers ProcLens already has. No, for anything that claims to
recognise malware or find "unusual behaviour" in general: there is nothing here to train that on.
**The thresholds cannot be chosen honestly yet**, because no data from real use has been looked at.
That is the main reason the log came first: a few days of it is the evidence to tune and test
against.

## What it could look at (all available today)

| Signal | Source | Needs collector |
|---|---|---|
| Memory in use, available, commit as a share of the limit | memory topic | yes |
| How fast available memory is falling | same, over time | yes |
| Network receive and send rate per adapter | app (adapter counters) | no |
| Errors and drops rising on an adapter | app | no |
| Driver CPU share | kernel trace | yes, and the trace |
| Busiest program by CPU or memory | process list | yes |
| Collector stopped without being asked; a self-check failed | app and profile | n/a |

## Two kinds of check

1. **Fixed rules**: true on any PC. Examples: memory 90% or more in use for 30 seconds; commit 90%
   or more of the limit; less than 1 GB available; an adapter's error or drop count rising by more
   than a few per second; driver CPU share over 10% for a minute. Cheap, explainable, low surprise.
2. **Learned baseline**: "unusual *for this PC at this time*". Keep a rolling window (say the last
   30 minutes) per metric, take the median and the median absolute deviation, and flag a value
   that is far outside it (for example more than 6 deviations) **and** above an absolute floor, for
   several samples in a row, with a cool-down so one event is one alert. The floor matters: a
   baseline of 0.1 MB/s makes 2 MB/s look 20 times "normal", which nobody needs to hear about.

## Why not something cleverer

- Nothing labels what is wrong, so there is nothing to train or score a model against.
- A model that cannot say why it fired is useless on a screen meant for a person: every alert must
  read as a sentence ("Network send is 12 MB/s, about 6 times its usual 2 MB/s over the last half hour").
- This PC uploads at around 13 MB/s for long stretches (seen during testing). A rule that treats that
  as abnormal would fire all day; one that learns it as normal would stay quiet. The baseline has to
  handle exactly this, and it can only be checked against real logs.

## Proposed first version

- Where: a small Rust module in the app, fed the same numbers as the logger (so it works with the
  window minimised and can write `"event":"anomaly"` lines into the log, which `Logger::event`
  already supports).
- What it shows: a "Needs attention" list on the Overview page: when, what, why, in a sentence, with
  a link to the relevant tab. Empty most of the time, and saying so.
- Cold start: no learned alerts until about 10 minutes of data exist in the session; fixed rules work
  at once.
- Tuning: replay recorded logs through the detector in a test, and count how many alerts a normal
  day produces. A rule that fires more than a few times a day without a real cause gets a higher
  floor or goes. This needs the user to run the log for a few typical days first.
- Not in the first version: Windows notifications, learning across days, per-program network
  baselines (per-program network traffic is not built yet), anything that acts on an alert.

## Decisions needed

1. **Fixed rules first, learned baselines second** (my recommendation), or both together?
2. **Where alerts appear:** the Overview list only, or also a Windows notification when ProcLens is
   minimised? (Notifications raise the cost of a false alarm.)
3. **Willing to run the log for a few typical days** so the thresholds are set from your PC's real
   behaviour instead of guesses? Until then any number I picked would be a guess.
