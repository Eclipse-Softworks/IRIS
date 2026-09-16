import sys
import time

def align_sequence(ref_len: int, read_len: int, match_score: int, mismatch_score: int, gap_penalty: int) -> int:
    width = read_len + 1
    height = ref_len + 1
    total_cells = width * height

    dp = [0] * total_cells

    for r in range(ref_len + 1):
        dp[r * width + 0] = r * gap_penalty

    for c in range(read_len + 1):
        dp[0 * width + c] = c * gap_penalty

    for i in range(1, ref_len + 1):
        for j in range(1, read_len + 1):
            is_match = ((i * 7 + j * 3) % 11) == 0
            if is_match:
                score_diag = dp[(i - 1) * width + (j - 1)] + match_score
            else:
                score_diag = dp[(i - 1) * width + (j - 1)] + mismatch_score

            score_up = dp[(i - 1) * width + j] + gap_penalty
            score_left = dp[i * width + (j - 1)] + gap_penalty

            best = score_diag
            if score_up > best:
                best = score_up
            if score_left > best:
                best = score_left

            dp[i * width + j] = best

    return dp[ref_len * width + read_len]

def main():
    ref_len = 120
    read_len = 120
    num_patients = 100

    total_alignment_score = 0
    for patient in range(num_patients):
        score = align_sequence(ref_len, read_len, 2, -1, -2)
        total_alignment_score += score

    print(f"Processed {num_patients} patient biopsy alignments (14,641 matrix cells each).")
    print(f"Average Alignment Score: {total_alignment_score // num_patients}")

if __name__ == "__main__":
    main()
