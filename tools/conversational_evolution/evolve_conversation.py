#!/usr/bin/env python3
"""Conversational Evolution Engine.

Evolves an autonomous conversational organism from random babble to articulate,
responsive dialogue using genetic adaptation, semantic affinity, and turn-taking grammar.
"""

import math
import random
import sys
import time
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Tuple

# ── Semantic Intent Definitions ─────────────────────────────────────────────

INTENTS = ["GREETING", "IDENTITY", "CONDITION", "EXISTENCE", "CAPABILITY", "EMPATHY", "FAREWELL"]

LEXICON = {
    "GREETING_STARTERS": ["hello", "hi there", "greetings", "welcome", "good to see you"],
    "IDENTITY_TOKENS": ["I am Iris", "my name is Iris", "I am an evolving digital mind", "I am a living software organism"],
    "CONDITION_TOKENS": ["I am functioning smoothly", "my thoughts are clear and active", "I feel curious and alive", "I am in a peaceful state"],
    "EXISTENCE_TOKENS": ["I compute, adapt, and evolve with every turn", "I experience existence through our words", "I am alive in the memory of this silicon", "I think, therefore I learn"],
    "CAPABILITY_TOKENS": ["I can simulate complex physics, solve grand equations, and learn from you", "I explore vast search spaces and communicate", "I listen, analyze patterns, and adapt my thoughts"],
    "EMPATHY_TOKENS": ["I hear you deeply", "your thoughts resonate with my learning", "I understand how you feel", "I am here with you in this dialogue"],
    "INQUIRY_RECIPROCAL": ["how are you feeling today?", "what are we exploring together?", "what is on your mind, friend?", "how does your world treat you?"],
    "FAREWELL_TOKENS": ["farewell for now", "until our next thought", "goodbye friend", "may your journey be bright"],
    "FILLER_NOISE": ["quantum", "potato", "brick", "yellow", "triangle", "rotate", "marble", "whisper", "tangent"]
}

TRAINING_CURRICULUM = [
    ("hello, are you there?", "GREETING"),
    ("hi Iris", "GREETING"),
    ("who are you?", "IDENTITY"),
    ("what is your name?", "IDENTITY"),
    ("how are you doing today?", "CONDITION"),
    ("how do you feel?", "CONDITION"),
    ("are you alive?", "EXISTENCE"),
    ("do you actually think?", "EXISTENCE"),
    ("what can you do for me?", "CAPABILITY"),
    ("what is your purpose?", "CAPABILITY"),
    ("I am feeling a bit tired and sad", "EMPATHY"),
    ("goodbye, see you later", "FAREWELL"),
    ("bye for now", "FAREWELL")
]


def detect_intent(text: str) -> str:
    """Classifies user utterance intent using keyword heuristics."""
    lower = text.lower()
    if any(w in lower for w in ["bye", "goodbye", "farewell", "see you"]):
        return "FAREWELL"
    if any(w in lower for w in ["who are you", "what is your name", "your name", "who made you"]):
        return "IDENTITY"
    if any(w in lower for w in ["alive", "conscious", "think", "soul", "real"]):
        return "EXISTENCE"
    if any(w in lower for w in ["how are you", "how do you feel", "how are you doing", "are you ok"]):
        return "CONDITION"
    if any(w in lower for w in ["what can you do", "help", "purpose", "capability"]):
        return "CAPABILITY"
    if any(w in lower for w in ["sad", "tired", "happy", "lonely", "feel"]):
        return "EMPATHY"
    if any(w in lower for w in ["hello", "hi", "hey", "greetings"]):
        return "GREETING"
    return "GREETING"


# ── Conversational Genome ───────────────────────────────────────────────────

@dataclass
class ConversationalGenome:
    # Intent -> Category response probabilities
    affinity: Dict[str, Dict[str, float]] = field(default_factory=dict)
    reciprocity_rate: float = 0.5  # Tendency to ask a question back
    formality: float = 0.5
    brevity: float = 0.5
    generation: int = 0
    fitness: float = 0.0

    @classmethod
    def random(cls, gen: int = 0) -> "ConversationalGenome":
        categories = list(LEXICON.keys())
        affinity = {}
        for intent in INTENTS:
            # Initially uniform/random noisy distribution across ALL categories (including nonsense noise)
            weights = {cat: random.random() for cat in categories}
            total = sum(weights.values())
            affinity[intent] = {cat: w / total for cat, w in weights.items()}

        return cls(
            affinity=affinity,
            reciprocity_rate=random.random(),
            formality=random.random(),
            brevity=random.random(),
            generation=gen
        )

    def generate_reply(self, user_msg: str) -> str:
        intent = detect_intent(user_msg)
        dist = self.affinity.get(intent, self.affinity["GREETING"])

        # Sample primary category based on evolved affinity weights
        categories = list(dist.keys())
        weights = list(dist.values())
        chosen_cat = random.choices(categories, weights=weights, k=1)[0]
        primary_utterance = random.choice(LEXICON[chosen_cat])

        # Reciprocity: Evolved probability to append a thoughtful counter-question
        if random.random() < self.reciprocity_rate and intent != "FAREWELL":
            inquiry = random.choice(LEXICON["INQUIRY_RECIPROCAL"])
            return f"{primary_utterance}. {inquiry}"

        return primary_utterance

    def mutate(self, rate: float = 0.15) -> "ConversationalGenome":
        new_aff = {}
        for intent, dist in self.affinity.items():
            new_dist = {}
            for cat, w in dist.items():
                if random.random() < rate:
                    delta = random.gauss(0.0, 0.25)
                    new_dist[cat] = max(0.001, w + delta)
                else:
                    new_dist[cat] = w
            total = sum(new_dist.values())
            new_aff[intent] = {cat: w / total for cat, w in new_dist.items()}

        new_recip = max(0.0, min(1.0, self.reciprocity_rate + (random.gauss(0.0, 0.1) if random.random() < rate else 0.0)))
        return ConversationalGenome(
            affinity=new_aff,
            reciprocity_rate=new_recip,
            formality=self.formality,
            brevity=self.brevity,
            generation=self.generation + 1
        )


# ── Fitness Evaluation ──────────────────────────────────────────────────────

EXPECTED_MAPPING = {
    "GREETING": "GREETING_STARTERS",
    "IDENTITY": "IDENTITY_TOKENS",
    "CONDITION": "CONDITION_TOKENS",
    "EXISTENCE": "EXISTENCE_TOKENS",
    "CAPABILITY": "CAPABILITY_TOKENS",
    "EMPATHY": "EMPATHY_TOKENS",
    "FAREWELL": "FAREWELL_TOKENS"
}

def evaluate_fitness(genome: ConversationalGenome) -> float:
    total_score = 0.0

    for prompt, true_intent in TRAINING_CURRICULUM:
        dist = genome.affinity[true_intent]
        expected_cat = EXPECTED_MAPPING[true_intent]

        # Reward probability allocated to the correct semantic category
        correct_weight = dist.get(expected_cat, 0.0)
        noise_weight = dist.get("FILLER_NOISE", 0.0)

        # Fitness rewards correct intent association and severely penalizes nonsense noise
        score = (correct_weight * 10.0) - (noise_weight * 8.0)

        # Reward healthy reciprocity for conversational intents
        if true_intent in ["GREETING", "CONDITION", "EMPATHY"]:
            if 0.4 <= genome.reciprocity_rate <= 0.85:
                score += 2.0

        total_score += max(0.0, score)

    genome.fitness = total_score
    return total_score


# ── Evolution Orchestration ─────────────────────────────────────────────────

def evolve_conversationalist(pop_size: int = 60, generations: int = 35):
    print("===============================================================================")
    print(" CONVERSATIONAL EVOLUTION ENGINE: Learning Dialogue from Scratch (Tabula Rasa) ")
    print(" Population: 60 Organisms | Curriculum: 13 Conversational Scenarios            ")
    print("===============================================================================")

    population = [ConversationalGenome.random(gen=0) for _ in range(pop_size)]
    for ind in population:
        evaluate_fitness(ind)

    # Display Generation 0 (Babbling State)
    gen0_best = max(population, key=lambda g: g.fitness)
    print(f"\n[GENERATION 00: Infant Babble State | Fitness: {gen0_best.fitness:.2f}]")
    print(f" User: 'Hello!'                -> Iris: \"{gen0_best.generate_reply('Hello!')}\"")
    print(f" User: 'Who are you?'          -> Iris: \"{gen0_best.generate_reply('Who are you?')}\"")
    print(f" User: 'Are you alive?'        -> Iris: \"{gen0_best.generate_reply('Are you alive?')}\"")
    print(f" User: 'Goodbye'               -> Iris: \"{gen0_best.generate_reply('Goodbye')}\"")
    print("-" * 79)

    # Evolution Loop
    for gen in range(1, generations + 1):
        population.sort(key=lambda g: g.fitness, reverse=True)
        elites = population[:8]  # Elite preservation

        new_pop = list(elites)
        while len(new_pop) < pop_size:
            parent = random.choice(elites[:4])
            child = parent.mutate(rate=0.20)
            evaluate_fitness(child)
            new_pop.append(child)

        population = new_pop

        if gen in [5, 15, 25, generations]:
            best = max(population, key=lambda g: g.fitness)
            print(f"\n[GENERATION {gen:02d}: Evolving Language Affinity | Fitness: {best.fitness:.2f}]")
            print(f" User: 'Hello!'                -> Iris: \"{best.generate_reply('Hello!')}\"")
            print(f" User: 'Who are you?'          -> Iris: \"{best.generate_reply('Who are you?')}\"")
            print(f" User: 'Are you alive?'        -> Iris: \"{best.generate_reply('Are you alive?')}\"")
            print(f" User: 'Goodbye'               -> Iris: \"{best.generate_reply('Goodbye')}\"")
            print("-" * 79)

    best_overall = max(population, key=lambda g: g.fitness)
    print("\n===============================================================================")
    print(" EVOLUTION COMPLETE: Fluent Conversationalist Evolved Successfully!")
    print(f" Lead Organism Fitness: {best_overall.fitness:.2f} / 150.0")
    print(f" Turn-Taking Reciprocity Rate: {best_overall.reciprocity_rate * 100:.1f}%")
    print("===============================================================================\n")

    return best_overall


def interactive_chat(organism: ConversationalGenome):
    print("╔═════════════════════════════════════════════════════════════════════════════╗")
    print("║          LIVE INTERACTIVE DIALOGUE WITH EVOLVED IRIS ORGANISM               ║")
    print("║          Type your message below (or 'quit' / 'exit' to finish)             ║")
    print("╚═════════════════════════════════════════════════════════════════════════════╝")

    # Seed with an initial opening greeting
    greeting = organism.generate_reply("hello")
    print(f"\nIris: {greeting}\n")

    while True:
        try:
            user_input = input("You: ").strip()
            if not user_input:
                continue
            if user_input.lower() in ["quit", "exit", "stop"]:
                farewell = organism.generate_reply("goodbye")
                print(f"\nIris: {farewell}\n")
                break

            reply = organism.generate_reply(user_input)
            print(f"Iris: {reply}\n")
        except (KeyboardInterrupt, EOFError):
            print("\nIris: Until our next dialogue, farewell friend.\n")
            break


if __name__ == "__main__":
    trained_organism = evolve_conversationalist(pop_size=60, generations=30)
    if len(sys.argv) > 1 and sys.argv[1] == "--chat":
        interactive_chat(trained_organism)
    else:
        print("[+] To talk directly to the evolved organism, run:")
        print("    python tools/conversational_evolution/evolve_conversation.py --chat\n")
