#!/usr/bin/env python3
"""Neural Conversational Language Model & Dataset Trainer.

Builds and trains a Recurrent Neural Network (RNN) Language Model from scratch
on a conversational dialogue dataset using Backpropagation Through Time (BPTT) and Adam.
Demonstrates the complete deep learning training pipeline:
Tokenization -> Dense Embeddings -> Recurrent Cell -> Cross-Entropy Loss -> Gradient Updates.
"""

import math
import os
import random
import re
import sys
import time
from typing import Dict, List, Optional, Tuple
import numpy as np

DATASET_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "dialogue_dataset.txt")


# ── 1. Tokenizer & Dataset Pipeline ─────────────────────────────────────────

class DialogueDataset:
    def __init__(self, filepath: str):
        self.raw_text = open(filepath, "r", encoding="utf-8").read()
        self.dialogue_pairs = self._parse_pairs(self.raw_text)

        # Build vocabulary
        tokens = ["<pad>", "<unk>", "<bos>", "<eos>"]
        for user_msg, iris_reply in self.dialogue_pairs:
            tokens.extend(self._tokenize(user_msg))
            tokens.extend(self._tokenize(iris_reply))

        self.vocab = sorted(list(set(tokens)))
        self.word2idx = {w: i for i, w in enumerate(self.vocab)}
        self.idx2word = {i: w for i, w in enumerate(self.vocab)}
        self.vocab_size = len(self.vocab)

    def _tokenize(self, text: str) -> List[str]:
        # Simple word + punctuation regex tokenizer
        text = text.lower()
        return re.findall(r"\b\w+\b|[?,.!]", text)

    def _parse_pairs(self, text: str) -> List[Tuple[str, str]]:
        pairs = []
        blocks = text.strip().split("\n\n")
        for block in blocks:
            lines = [l.strip() for l in block.splitlines() if l.strip()]
            user_line, iris_line = "", ""
            for l in lines:
                if l.startswith("User:"):
                    user_line = l[5:].strip()
                elif l.startswith("Iris:"):
                    iris_line = l[5:].strip()
            if user_line and iris_line:
                pairs.append((user_line, iris_line))
        return pairs

    def encode(self, text: str, add_special: bool = True) -> List[int]:
        tokens = self._tokenize(text)
        ids = [self.word2idx.get(t, self.word2idx["<unk>"]) for t in tokens]
        if add_special:
            return [self.word2idx["<bos>"]] + ids + [self.word2idx["<eos>"]]
        return ids

    def decode(self, ids: List[int]) -> str:
        words = []
        for i in ids:
            w = self.idx2word.get(i, "<unk>")
            if w in ["<pad>", "<bos>", "<eos>"]:
                continue
            words.append(w)
        # Format punctuation
        out = " ".join(words)
        out = re.sub(r"\s+([?,.!])", r"\1", out)
        return out.capitalize()


# ── 2. Neural Recurrent Language Model (NumPy Deep Learning Engine) ──────────

class NeuralLanguageModel:
    def __init__(self, vocab_size: int, embed_dim: int = 48, hidden_dim: int = 64):
        self.vocab_size = vocab_size
        self.embed_dim = embed_dim
        self.hidden_dim = hidden_dim

        # Xavier/Glorot weight initialization
        np.random.seed(42)
        scale_emb = 1.0 / math.sqrt(embed_dim)
        scale_ih = 1.0 / math.sqrt(hidden_dim)
        scale_hh = 1.0 / math.sqrt(hidden_dim)
        scale_ho = 1.0 / math.sqrt(vocab_size)

        # Trainable Parameters
        self.W_emb = np.random.randn(vocab_size, embed_dim) * scale_emb  # Word embeddings
        self.W_ih = np.random.randn(hidden_dim, embed_dim) * scale_ih    # Input-to-hidden
        self.W_hh = np.random.randn(hidden_dim, hidden_dim) * scale_hh   # Hidden-to-hidden
        self.b_h = np.zeros((hidden_dim, 1))                             # Hidden bias
        self.W_ho = np.random.randn(vocab_size, hidden_dim) * scale_ho   # Hidden-to-output
        self.b_o = np.zeros((vocab_size, 1))                             # Output bias

        # Adam Optimizer Moment Buffers
        self.m = {k: np.zeros_like(v) for k, v in self.get_params().items()}
        self.v = {k: np.zeros_like(v) for k, v in self.get_params().items()}
        self.t = 0

    def get_params(self) -> Dict[str, np.ndarray]:
        return {
            "W_emb": self.W_emb,
            "W_ih": self.W_ih,
            "W_hh": self.W_hh,
            "b_h": self.b_h,
            "W_ho": self.W_ho,
            "b_o": self.b_o
        }

    def forward(self, input_ids: List[int]) -> Tuple[List[np.ndarray], List[np.ndarray], List[np.ndarray]]:
        """Forward pass through time: computes embeddings, hidden states, and logits."""
        T = len(input_ids)
        h_states = [np.zeros((self.hidden_dim, 1))]
        logits = []
        probs = []

        for t in range(T):
            idx = input_ids[t]
            x_emb = self.W_emb[idx:idx+1].T  # (embed_dim, 1)

            # Recurrent hidden activation: h_t = tanh(W_ih * x_t + W_hh * h_{t-1} + b_h)
            h_prev = h_states[-1]
            h_t = np.tanh(np.dot(self.W_ih, x_emb) + np.dot(self.W_hh, h_prev) + self.b_h)
            h_states.append(h_t)

            # Output projection: z_t = W_ho * h_t + b_o
            z_t = np.dot(self.W_ho, h_t) + self.b_o
            logits.append(z_t)

            # Numerically stable Softmax
            exp_z = np.exp(z_t - np.max(z_t))
            p_t = exp_z / np.sum(exp_z)
            probs.append(p_t)

        return h_states, logits, probs

    def backward(self, input_ids: List[int], target_ids: List[int],
                 h_states: List[np.ndarray], probs: List[np.ndarray]) -> Tuple[float, Dict[str, np.ndarray]]:
        """Backpropagation Through Time (BPTT) with Cross-Entropy Loss."""
        T = len(target_ids)
        loss = 0.0

        dW_emb = np.zeros_like(self.W_emb)
        dW_ih = np.zeros_like(self.W_ih)
        dW_hh = np.zeros_like(self.W_hh)
        db_h = np.zeros_like(self.b_h)
        dW_ho = np.zeros_like(self.W_ho)
        db_o = np.zeros_like(self.b_o)

        dh_next = np.zeros((self.hidden_dim, 1))

        # Backward through time
        for t in reversed(range(T)):
            target = target_ids[t]
            p = probs[t]

            # Cross-Entropy Loss: -log(P(target))
            prob_target = max(p[target, 0], 1e-12)
            loss -= math.log(prob_target)

            # Output gradient: dL/dz = p - y_one_hot
            dz = p.copy()
            dz[target, 0] -= 1.0

            # Gradient to output weights
            h_t = h_states[t + 1]
            dW_ho += np.dot(dz, h_t.T)
            db_o += dz

            # Gradient to hidden state
            dh = np.dot(self.W_ho.T, dz) + dh_next
            # Backprop through tanh: d/dx tanh(x) = 1 - tanh^2(x)
            dtanh = (1.0 - h_t ** 2) * dh

            db_h += dtanh
            x_emb = self.W_emb[input_ids[t]:input_ids[t]+1].T
            dW_ih += np.dot(dtanh, x_emb.T)
            h_prev = h_states[t]
            dW_hh += np.dot(dtanh, h_prev.T)

            # Gradient to embedding
            dx_emb = np.dot(self.W_ih.T, dtanh)
            dW_emb[input_ids[t]:input_ids[t]+1] += dx_emb.T

            dh_next = np.dot(self.W_hh.T, dtanh)

        loss /= T
        grads = {
            "W_emb": dW_emb,
            "W_ih": dW_ih,
            "W_hh": dW_hh,
            "b_h": db_h,
            "W_ho": dW_ho,
            "b_o": db_o
        }
        return loss, grads

    def step_adam(self, grads: Dict[str, np.ndarray], lr: float = 0.01,
                  beta1: float = 0.9, beta2: float = 0.999, eps: float = 1e-8, clip: float = 5.0):
        """Adam optimizer step with gradient clipping."""
        self.t += 1
        params = self.get_params()

        for k in params:
            # Gradient clipping
            g = np.clip(grads[k], -clip, clip)

            # Biased first and second moment estimates
            self.m[k] = beta1 * self.m[k] + (1.0 - beta1) * g
            self.v[k] = beta2 * self.v[k] + (1.0 - beta2) * (g ** 2)

            # Bias-corrected moments
            m_hat = self.m[k] / (1.0 - beta1 ** self.t)
            v_hat = self.v[k] / (1.0 - beta2 ** self.t)

            # Parameter update
            params[k] -= lr * m_hat / (np.sqrt(v_hat) + eps)

    def generate_response(self, user_prompt: str, dataset: DialogueDataset, max_len: int = 25, temperature: float = 0.7) -> str:
        """Autoregressively predicts next tokens given a user prompt."""
        # Encode user prompt
        prompt_ids = dataset.encode(f"User: {user_prompt}", add_special=False)
        bos_id = dataset.word2idx["<bos>"]
        eos_id = dataset.word2idx["<eos>"]

        # Prime the recurrent hidden state with the prompt
        h = np.zeros((self.hidden_dim, 1))
        for p_id in prompt_ids:
            x_emb = self.W_emb[p_id:p_id+1].T
            h = np.tanh(np.dot(self.W_ih, x_emb) + np.dot(self.W_hh, h) + self.b_h)

        # Autoregressive generation of Iris's reply
        curr_id = bos_id
        generated_ids = []

        for _ in range(max_len):
            x_emb = self.W_emb[curr_id:curr_id+1].T
            h = np.tanh(np.dot(self.W_ih, x_emb) + np.dot(self.W_hh, h) + self.b_h)
            z = np.dot(self.W_ho, h) + self.b_o

            # Temperature-scaled Softmax
            logits = z / max(temperature, 0.1)
            exp_z = np.exp(logits - np.max(logits))
            probs = (exp_z / np.sum(exp_z)).flatten()

            # Exclude special tokens from early prediction
            probs[dataset.word2idx["<pad>"]] = 0.0
            probs[dataset.word2idx["<unk>"]] = 0.0
            probs[dataset.word2idx["<bos>"]] = 0.0
            total_p = np.sum(probs)
            if total_p > 0:
                probs /= total_p
            else:
                break

            # Sample next token from probability distribution
            next_id = np.random.choice(self.vocab_size, p=probs)
            if next_id == eos_id:
                break

            generated_ids.append(next_id)
            curr_id = next_id

        return dataset.decode(generated_ids)


# ── 3. Training Loop & Interactive Demo ──────────────────────────────────────

def train_model(epochs: int = 80, lr: float = 0.015):
    dataset = DialogueDataset(DATASET_PATH)
    total_words = sum(len(p[0].split()) + len(p[1].split()) for p in dataset.dialogue_pairs)
    print("===============================================================================")
    print(" NEURAL CONVERSATIONAL LANGUAGE MODEL TRAINER")
    print(f" Dataset: {len(dataset.dialogue_pairs)} Dialogue Pairs ({total_words} words) | Vocab Size: {dataset.vocab_size} tokens")
    print(" Architecture: Word Embedding (d=48) -> Recurrent Cell (h=64) -> Softmax LM")
    print(" Optimizer: Backpropagation Through Time (BPTT) + Adam Optimizer")
    print("===============================================================================")

    model = NeuralLanguageModel(vocab_size=dataset.vocab_size, embed_dim=48, hidden_dim=64)

    # Convert dataset pairs into training sequences: [User prompt] -> [Iris reply]
    training_data = []
    for user_msg, iris_reply in dataset.dialogue_pairs:
        full_text = f"User: {user_msg} Iris: {iris_reply}"
        seq = dataset.encode(full_text, add_special=True)
        inp = seq[:-1]
        tgt = seq[1:]
        training_data.append((inp, tgt))

    # Initial Untrained Generation Sample
    print("\n[EPOCH 00: Untrained Random Weights | Untrained State]")
    print(f" Prompt: 'who are you?'   -> Iris: \"{model.generate_response('who are you?', dataset)}\"")
    print(f" Prompt: 'are you alive?' -> Iris: \"{model.generate_response('are you alive?', dataset)}\"")
    print("-" * 79)

    t0 = time.perf_counter()

    for epoch in range(1, epochs + 1):
        epoch_loss = 0.0
        random.shuffle(training_data)

        for inp_seq, tgt_seq in training_data:
            h_states, logits, probs = model.forward(inp_seq)
            loss, grads = model.backward(inp_seq, tgt_seq, h_states, probs)
            model.step_adam(grads, lr=lr)
            epoch_loss += loss

        avg_loss = epoch_loss / len(training_data)
        perplexity = math.exp(min(avg_loss, 20.0))

        if epoch in [15, 35, 60, epochs]:
            print(f"[EPOCH {epoch:02d} / {epochs}] Cross-Entropy Loss: {avg_loss:6.4f} | Perplexity: {perplexity:6.2f}")
            sample_who = model.generate_response("who are you?", dataset, temperature=0.5)
            sample_alive = model.generate_response("are you alive?", dataset, temperature=0.5)
            print(f"  Prompt: 'who are you?'   -> Iris: \"{sample_who}\"")
            print(f"  Prompt: 'are you alive?' -> Iris: \"{sample_alive}\"")
            print("-" * 79)

    elapsed = time.perf_counter() - t0
    print(f"\n[+] Neural Training Completed in {elapsed:.2f}s ({epochs / elapsed:.1f} epochs/sec).")
    print("===============================================================================\n")

    return model, dataset


def interactive_chat(model: NeuralLanguageModel, dataset: DialogueDataset):
    print("╔═════════════════════════════════════════════════════════════════════════════╗")
    print("║          LIVE INTERACTIVE CHAT WITH TRAINED NEURAL IRIS MODEL               ║")
    print("║          Type your message below (or 'quit' / 'exit' to exit)               ║")
    print("╚═════════════════════════════════════════════════════════════════════════════╝")

    greeting = model.generate_response("hello", dataset, temperature=0.6)
    print(f"\nIris: {greeting}\n")

    while True:
        try:
            user_msg = input("You: ").strip()
            if not user_msg:
                continue
            if user_msg.lower() in ["quit", "exit", "stop"]:
                bye = model.generate_response("goodbye", dataset, temperature=0.6)
                print(f"\nIris: {bye}\n")
                break

            reply = model.generate_response(user_msg, dataset, temperature=0.6)
            print(f"Iris: {reply}\n")
        except (KeyboardInterrupt, EOFError):
            print("\nIris: Goodbye friend.\n")
            break


if __name__ == "__main__":
    trained_model, ds = train_model(epochs=80, lr=0.015)
    if len(sys.argv) > 1 and sys.argv[1] == "--chat":
        interactive_chat(trained_model, ds)
    else:
        print("[+] To chat directly with the trained neural model, run:")
        print("    python tools/neural_dialogue/train_dialogue_model.py --chat\n")
