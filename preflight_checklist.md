Code Readiness:
□ Bot connects to Binance production (verified: can read order book)
□ Bot connects to Arbitrum (verified: can read pool reserves)
□ Fee calculation uses real fees (CEX 10bps, DEX 30bps, gas)
□ Risk limits configured for $100 capital
□ Kill switch tested (created file → bot stopped)
□ Circuit breaker tested
□ Safety constants hardcoded (ABSOLUTE_MAX values)
□ Dry run completed (30+ minutes of logs attached)

Security:
□ API key: Spot Trading only
□ API key: IP whitelist set
□ API key: NO withdrawal permission
□ .env file listed in .gitignore
□ No secrets in git history (git log checked)

Operational:
□ Logging writes to files
□ Telegram alerts working (or alternative monitoring)
□ Know how to read logs
□ Emergency flatten procedure documented
□ Have Binance app/web ready for manual intervention

Student signature: Korzh O.D  Date: 18.02.26
Instructor sign-off: ______________  Date: ________
