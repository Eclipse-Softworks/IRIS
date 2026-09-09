# Beginner: checked ledger

A two-module ledger stores money as integer cents, rejects invalid debits and overdrafts, and checks that failed operations leave the balance unchanged.

## Run

```text
iris run projects/ledger/main.iris
```

No service, credentials, or files. Expected balance: 7500 cents.

## Verify

```text
python tools/verify_learning.py --filter projects/ledger/
```

The catalog runner checks the native and interpreter paths separately. Every
assertion is part of the program, so incorrect results fail the run.

## Extend

Try adding a deposit operation and test that it rejects negative amounts.
