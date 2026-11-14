# Gridworld agent: active inference, learned online

An agent that starts knowing nothing about a 5 × 5 grid (where the walls
are, that mud makes a move fail half the time) and learns it while it acts.
When the world changes under it, it relearns.

```text
S . . # .
. ~ ~ # .        S start, G goal, # wall, ~ mud
. ~ ~ . .
. . # . .
# . . . G
```

| File | Role |
| --- | --- |
| [world.iris](world.iris) | The environment: walls, mud, a seeded Park-Miller generator, and a wall that can appear mid-run |
| [agent.iris](agent.iris) | The agent: a Dirichlet model of the dynamics, learned online with forgetting, and action selection by expected free energy |
| [run.iris](run.iris) | Learning, then a change to the world, against a random walk and the best possible policy |

**How it decides.** For every (cell, action) the agent keeps a Dirichlet
distribution over the five outcomes (stay, or move up, right, down or left).
The prior says a move usually goes where it is aimed. Each move it makes adds
a count, after that pair's older evidence decays slightly, so the model can
follow a world that changes. It acts to minimize expected free energy:

```text
G(s, a) = E[risk] - 0.5 * novelty(s, a) + E[G*(s')]
```

Risk is one nat for every step that does not end at the goal (the preferred,
absorbing outcome). Novelty is the expected information gain about the
Dirichlet parameters, `0.5 (n - 1) / a0`, which falls as a pair is tried; it
is what makes the agent explore. `G*` comes from value iteration on the
learned model, refined by one sweep per decision.

`std.ais` gains the one-step, model-based form of the same quantity
(`model_expected_free_energy`, `active_inference_choose`); see
[known issue #81](../../docs/known-issues.md) for why.

## Run

```text
iris run projects/gridworld_inference/run.iris
```

## What was observed (2026-10-19, Windows)

Identical output on both backends (seeded, so every decision is the same).
1.7 s natively, 36 s in the debug-build interpreter.

```text
best possible: 8.0 expected steps
random walk: 73.7 steps on average (capped at 100)
learning, steps per episode: 100 26 8 9 10 8 8 8 8 8 11 8 8 8 8 8 8 8 8 8
then exploiting: 8.0 steps on average over 20 episodes
cell 15 walled off; best possible now: 9.0 expected steps
relearning, steps per episode: 13 14 11 8 8 9 8 9 14 11 8 8 8 10 8
then exploiting: 8.8 steps on average over 20 episodes
learned chance a move fails in the mud (true 0.5): 6: 0.4 (7.8 recent moves)  7: 0.5 (16.6 recent moves)  11: 0.5 (23.3 recent moves)  12: 0.4 (14.4 recent moves)
```

The first episode is all exploration and ends at the 100-step cap. By the
third episode the agent takes the best route, down the left column, and
exploiting what it has learned matches the best possible policy (8.0). When
the left column is walled off, its next episodes are longer while it
relearns; it then goes through the mud and, exploiting, averages 8.8 steps
against a best possible 9.0 in expectation (the evaluation episodes were
lucky in the mud). Its estimate of how often the mud stops a move lies within
0.1 of the true 0.5 wherever it has enough evidence. The run asserts all of
these, with margins.

## Limits

- The agent sees its position exactly. There is no perceptual uncertainty,
  so the ambiguity term of expected free energy is zero here; only risk and
  novelty act.
- Novelty is weighted at 0.5 so that a step never pays for itself.
  Exploration therefore stops once evidence accumulates, and how much the
  agent explores depends on that weight and on the prior.
- One seeded run is shown; the margins in the asserts are not a statistical
  guarantee over seeds.
- A tabular agent for 25 cells. It learns the dynamics, not a policy that
  generalizes across states.
