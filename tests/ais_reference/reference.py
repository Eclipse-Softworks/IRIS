"""Reference values for tests/test_ais_reference.iris.

An independent implementation of the quantities std.ais computes, written
from the textbook definitions rather than from the IRIS code:

  information gain  sum_o q(o) KL(q(s | o) || q(s))   (IRIS: H[q(o)] - ambiguity)
  expected free energy  KL(q(o) || C) + E_q(s) H[A(. | s)]   (IRIS: -utility - info gain)
  novelty  sum_o q(o) sum_s q(s) (1 / a(o,s) - 1 / sum_o a(o,s))   (pymdp's spm_wnorm)
  recursive least squares  the closed-form ridge solution
                           (X'X + I / p0)^-1 X'y   (IRIS: one sample at a time)

Run it to regenerate the constants: python tests/ais_reference/reference.py
Plain Python, no packages.
"""
import math


def tmaze(p=0.8):
    ns, no, na = 8, 7, 4
    a = [[0.0] * ns for _ in range(no)]
    b = [[[0.0] * ns for _ in range(ns)] for _ in range(na)]  # b[act][s][s2]
    for ctx in range(2):
        a[0][0 * 2 + ctx] = 1.0
        a[1][1 * 2 + ctx] = 1.0 - ctx
        a[2][1 * 2 + ctx] = float(ctx)
        lw = p if ctx == 0 else 1 - p
        a[3][2 * 2 + ctx] = lw
        a[4][2 * 2 + ctx] = 1 - lw
        rw = p if ctx == 1 else 1 - p
        a[5][3 * 2 + ctx] = rw
        a[6][3 * 2 + ctx] = 1 - rw
        for act in range(na):
            for loc in range(4):
                dest = loc if loc >= 2 else act
                b[act][loc * 2 + ctx][dest * 2 + ctx] = 1.0
    scores = [0, 0, 0, 3, -3, 3, -3]
    mx = max(scores)
    lse = mx + math.log(sum(math.exp(s - mx) for s in scores))
    log_c = [s - lse for s in scores]
    d = [0.5, 0.5] + [0.0] * 6
    return ns, no, na, a, b, log_c, d


def predict(b, q, act):
    ns = len(q)
    return [sum(b[act][s][s2] * q[s] for s in range(ns)) for s2 in range(ns)]


def obs_probs(a, q):
    return [sum(row[s] * q[s] for s in range(len(q))) for row in a]


def posterior(a, prior, o):
    un = [a[o][s] * prior[s] for s in range(len(prior))]
    z = sum(un)
    return [u / z for u in un]


def kl(p, q):
    return sum(pi * (math.log(pi) - math.log(qi)) for pi, qi in zip(p, q) if pi > 0)


def entropy(p):
    return -sum(x * math.log(x) for x in p if x > 0)


def policy_value(a, b, log_c, q, policy):
    """Risk, ambiguity, information gain and G, each summed over the policy."""
    risk = amb = ig = 0.0
    qs = q
    c = [math.exp(x) for x in log_c]
    for act in policy:
        qs = predict(b, qs, act)
        qo = obs_probs(a, qs)
        risk += kl(qo, c)
        amb += sum(qs[s] * entropy([a[o][s] for o in range(len(a))]) for s in range(len(qs)))
        for o, p_o in enumerate(qo):
            if p_o > 0:
                ig += p_o * kl(posterior(a, qs, o), qs)
    return risk, amb, ig, risk + amb


def softmax(xs):
    mx = max(xs)
    es = [math.exp(x - mx) for x in xs]
    z = sum(es)
    return [e / z for e in es]


def main():
    ns, no, na, a, b, log_c, d = tmaze()
    q0 = posterior(a, d, 0)
    policies = [(x, y) for x in range(na) for y in range(na)]
    values = [policy_value(a, b, log_c, q0, p) for p in policies]
    print("// T-maze from the centre, two-step policies in lexicographic order:")
    print("// efe (= risk + ambiguity), info gain")
    for p, v in zip(policies, values):
        print(f"//   {p}: efe {v[3]!r} info_gain {v[2]!r}")
    gamma = 16.0
    probs = softmax([-gamma * v[3] for v in values])
    action = [sum(pr for pol, pr in zip(policies, probs) if pol[0] == act) for act in range(na)]
    print("val want_efe = list(" + ", ".join(repr(v[3]) for v in values) + ")")
    print("val want_ig = list(" + ", ".join(repr(v[2]) for v in values) + ")")
    print("val want_action = list(" + ", ".join(repr(x) for x in action) + ")")

    # After going to the cue and seeing "left".
    q1 = posterior(a, predict(b, q0, 1), 1)
    print("val want_belief = list(" + ", ".join(repr(x) for x in q1) + ")")
    v1 = [policy_value(a, b, log_c, q1, p) for p in policies]
    probs1 = softmax([-gamma * v[3] for v in v1])
    action1 = [sum(pr for pol, pr in zip(policies, probs1) if pol[0] == act) for act in range(na)]
    print("val want_action_after_cue = list(" + ", ".join(repr(x) for x in action1) + ")")

    # Novelty of a two-state model with counts [[2, 1], [1, 3]] (rows: o).
    counts = [[2.0, 1.0], [1.0, 3.0]]
    am = [[counts[o][s] / (counts[0][s] + counts[1][s]) for s in range(2)] for o in range(2)]
    qs = [0.25, 0.75]
    qo = obs_probs(am, qs)
    nov = 0.0
    for o in range(2):
        w = sum((1.0 / counts[o][s] - 1.0 / (counts[0][s] + counts[1][s])) * qs[s] for s in range(2))
        nov += qo[o] * w
    print("val want_novelty = " + repr(nov))

    # Ridge-regularised least squares, the limit RLS reaches with forgetting 1.
    xs, ys = [], []
    t = 30.0
    for i in range(40):
        u = float(i % 3 - 1)
        nxt = t + 0.1 * (-0.5 * (t - 20.0) + 15.0 * u)
        xs.append([t, u, 1.0])
        ys.append(nxt)
        t = nxt
    p0 = 100.0
    m = [[sum(x[r] * x[c] for x in xs) + (1.0 / p0 if r == c else 0.0) for c in range(3)] for r in range(3)]
    v = [sum(x[r] * y for x, y in zip(xs, ys)) for r in range(3)]
    w = solve(m, v)
    print("val want_rls = list(" + ", ".join(repr(x) for x in w) + ")")


def solve(m, v):
    n = len(v)
    aug = [row[:] + [v[i]] for i, row in enumerate(m)]
    for col in range(n):
        piv = max(range(col, n), key=lambda r: abs(aug[r][col]))
        aug[col], aug[piv] = aug[piv], aug[col]
        for r in range(n):
            if r != col:
                f = aug[r][col] / aug[col][col]
                for c in range(col, n + 1):
                    aug[r][c] -= f * aug[col][c]
    return [aug[i][n] / aug[i][i] for i in range(n)]


if __name__ == "__main__":
    main()
