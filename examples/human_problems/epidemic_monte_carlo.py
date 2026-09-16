import sys
import time

def simulate_one(intervention_day: int, beta_reduction_pct: int) -> int:
    total_pop = 1000000.0
    s = 999900.0
    e = 100.0
    i = 0.0
    h = 0.0
    r = 0.0
    d = 0.0

    icu_capacity = 2500.0
    base_beta = 0.35
    sigma = 0.20
    gamma = 0.10
    hosp_rate = 0.05
    hosp_recovery = 0.07

    for day in range(120):
        if day >= intervention_day:
            factor = float(100 - beta_reduction_pct)
            current_beta = base_beta * (factor / 100.0)
        else:
            current_beta = base_beta

        new_exposed = (current_beta * s * (e + i)) / total_pop
        new_infected = sigma * e
        new_hosp = hosp_rate * new_infected
        new_rec_mild = gamma * i
        new_rec_hosp = hosp_recovery * h

        mortality_rate = 0.12 if h > icu_capacity else 0.02
        new_deaths = mortality_rate * h

        s -= new_exposed
        e += new_exposed - new_infected
        i += new_infected - (new_rec_mild + new_hosp)
        h += new_hosp - (new_rec_hosp + new_deaths)
        r += new_rec_mild + new_rec_hosp
        d += new_deaths

    return int(d)

def main():
    num_simulations = 20000
    baseline_deaths = simulate_one(999, 0)

    best_saved = 0
    best_day = 0
    best_reduction = 0
    total_lives_saved_sum = 0

    for k in range(num_simulations):
        policy_day = 10 + (k % 50)
        policy_reduction = 20 + ((k * 7) % 50)

        deaths = simulate_one(policy_day, policy_reduction)
        saved = baseline_deaths - deaths
        total_lives_saved_sum += saved

        if saved > best_saved:
            best_saved = saved
            best_day = policy_day
            best_reduction = policy_reduction

    print(f"Simulated {num_simulations} public health policy scenarios.")
    print(f"Optimal Policy: Intervene on Day {best_day} with {best_reduction}% contact reduction.")
    print(f"Maximum Human Lives Saved: {best_saved}")
    print(f"Average Lives Saved across all policies: {total_lives_saved_sum // num_simulations}")

if __name__ == "__main__":
    main()
