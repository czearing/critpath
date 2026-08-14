# critpath

Finds why a program finished when it did.

A profiler sorts work by size. That answers a question nobody asked, because the largest thing on
the timeline is frequently work that could be deleted outright without the finish moving. critpath
recovers the **chain of dependent work that actually determined the end**, proves what is wrong
with that chain, and says how much room is left before fixing it stops paying.

It never starts your application, never drives a browser, and never learns a framework.

```
$ critpath fixtures/web-route.json --budget 1

The finish was decided by a chain of 5 activities lasting 155.0ms: 120.0ms working, 35.0ms waiting.
Shortening it by more than 65.0ms hands the constraint to a different chain.

The chain, in order:
  20.0ms ParseHTML [blink]
  60.0ms EvaluateScript [v8]
  15.0ms JSON.parse [v8]
  (35.0ms waiting)
  15.0ms JSON.parse [v8]
  10.0ms CommitLayerTree [cc]

What is provably wrong:
  JSON.parse ran 2 times on the chain; the repeats cost 15.0ms. Doing it once is worth that much.
  Nothing ran anywhere for 35.0ms before JSON.parse. That is a dependency issued later than it had to be, not slow code.
  The largest activity, TranscodeImage at 80.0ms, is not on the chain. Deleting it entirely would not move the finish.

Best 1 change(s), worth 35.0ms and proven optimal:
  Nothing ran anywhere for 35.0ms before JSON.parse. That is a dependency issued later than it had to be, not slow code.
```

The 80ms image transcode is the biggest bar in that trace, and it is worth nothing. Every ranked
profile puts it first.

## Why it works on any language

The input is [Trace Event Format][tef] — the JSON Chrome emits, and so do Go's execution tracer,
VizTracer, PerfMark, `tracing-chrome`, PyTorch, TensorFlow, Bazel and Hermes. Perfetto ingests it
directly. It is already the lingua franca of tracing, so critpath needed no adapters to be
universal.

That format matters for a second reason. Its **flow events** are literal dependency edges: a
producer that emits `ph: "s"` and `ph: "f"` sharing an `id` has *stated* that one piece of work
waited on another. The dependency graph arrives already drawn by the thing that knows the truth,
rather than being guessed at from timestamps here.

Nothing in the engine knows what React, a shader compiler or a build system is. An activity is a
named interval on a track; an edge is a stated dependency. Anything that can emit those two facts
can be diagnosed.

## Why it refuses

When the evidence cannot carry a verdict, critpath returns a refusal instead of a number. This is
the design, borrowed from [fitkit][fitkit], and it is what separates the tool from a linter that
guesses.

| Situation | Answer |
| --- | --- |
| Several tracks, and the trace never stated an order between them | **Refused.** Ranking them by duration would invent a causality nobody recorded. |
| Any event in the trace went unread, unpaired or unbound | **Refused.** Silence from an incomplete trace is indistinguishable from a clean result. |
| A dependency the trace's own timestamps deny | **Counted as a hole.** Choosing which half of the contradiction to disbelieve is not supported by evidence. |
| A begin event with no matching end | **Counted as a hole**, never invented as a zero-length activity. |
| Nothing on offer costs the chain any time | **Refused**, rather than ranking worthless repairs. |

A refusal is printed and exits clean. Not concluding is a result.

## No thresholds

There is not a single tuned number in the rule set, because a threshold is a claim about a machine,
a network and a workload that the trace never made. Each rule fires on a fact instead:

- **Repeated work** — the same category and name appear twice *on the chain*. Repetition is
  observable; "too slow" is not. Repeating work off the chain delays nothing and is deliberately
  not reported.
- **Dead wait** — the chain waited while *nothing ran anywhere in the trace*. Not contention, which
  is a different problem with a different fix. An idle machine is always a dependency issued later
  than it had to be.
- **Off-path dominance** — the largest activity is not on the chain, so it can be deleted without
  the finish moving. Stated because it is the finding a ranked profile inverts.

The one number the tool asks for is `--budget`: how many changes you can afford. It has no default,
because that is a fact about your team and not about your trace.

## Margin

Every answer carries how far it can be wrong before it stops holding. The margin is the gap between
when the chain finishes and when the *next* independent chain finishes. Beat that and something
else is the constraint, so the optimiser will not spend a change to buy time the schedule cannot
realise. **A chain with no margin selects no repairs at all** — the correct and unpopular answer.

## How it decides

`cost[v] = duration(v) + max over dependencies u of (cost[u] + wait(u, v))`

The chain *ends* where the trace says the work ended, which is observed rather than modelled, and
is then reconstructed backwards through the memoised best predecessor. Repair selection is a subset
optimisation over the findings under the budget, scored against the margin ceiling, reported as
proven when every combination was enumerated and honestly labelled a beam search when it was not.

## Layout

| Crate | Holds |
| --- | --- |
| `critpath-core` | The vocabulary: activity, track, edge, coverage. Knows no language and no format. |
| `critpath-trace` | The only crate that knows a wire format. |
| `critpath-graph` | The dynamic program, the ordering, and the margin. |
| `critpath-laws` | The rules, each gated behind what it needs before it may speak. |
| `critpath` | The facade, the report and the CLI. |

## Its relationship to fitkit

[fitkit][fitkit] supplies the discipline: `Refusal`, `Confidence`, `Margin`, the `Law` trait whose
gate cannot be bypassed, and `optimise_subset` for choosing repairs. The DAG longest-path is
**native to this repository** — fitkit's dynamic programming is Viterbi over a trellis and a subset
optimiser, neither of which is a longest path over a general graph. Saying otherwise would be the
kind of unearned claim this tool exists to refuse.

## Building

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p critpath -- fixtures/web-route.json --budget 1
```

The fixtures are seeded controls. Each carries a defect that must be named and a decoy that must
stay silent, so weakening a rule into firing on ordinary work fails the suite.

## Licence

MIT.

[tef]: https://docs.google.com/document/d/1CvAClvFfyA5R-PhYUmn5OOQtYMH4h6I0nSsKchNAySU/preview
[fitkit]: https://github.com/czearing/fitkit
