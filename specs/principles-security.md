# Security Principles

> Non-negotiable security requirements for protecting user data and system integrity.

## Philosophy

Security is not a feature—it is a constraint that shapes every decision. When security conflicts with convenience, security wins. When security conflicts with performance, security wins. The only thing that trumps security is user safety.

---

## Core Principles

### 1. Defense in Depth

No single security control should be the only barrier.

- Validate at input boundaries AND within business logic
- Use network-level restrictions AND application-level auth
- Log security events AND alert on anomalies
- Never rely on "the firewall will handle it"

### 2. Principle of Least Privilege

Every component should have the minimum permissions required.

- Request only necessary Workflowy API scopes
- Avoid storing credentials beyond immediate need
- Process data in memory; avoid unnecessary persistence
- Run with minimal filesystem permissions

### 3. Secure by Default

The safe option should require zero configuration.

- Sensitive data is never logged by default
- Debug modes require explicit opt-in
- Credentials are never echoed, even partially
- Error messages reveal nothing exploitable

### 4. Fail Secure

When something goes wrong, fail into a safe state.

- Authentication failures deny access (not grant partial)
- Corrupted config prevents startup (not uses defaults)
- Network errors abort operations (not continue without data)
- Unknown inputs are rejected (not sanitized and processed)

### 5. Trust Nothing External

All external input is hostile until validated.

- User input is hostile
- API responses are hostile
- Environment variables are hostile
- File contents are hostile

Validate schema, type, range, and format for all external data.

---

## Secrets Management

### What Constitutes a Secret

- API keys and tokens
- Passwords and credentials
- Session identifiers
- Personal identifiable information (PII)
- User content (notes are private by default)

### Handling Requirements

| Action | Requirement |
|--------|-------------|
| Storage | Environment variables only; never in code or config files |
| Logging | Never log secrets, even at debug level |
| Display | Never echo back, even partially masked |
| Memory | Clear from memory after use where practical |
| Transit | HTTPS only; no fallback to HTTP |
| Error messages | No secrets in error output, ever |

### Implementation

```typescript
// Logging credential presence without exposing value
logger.debug('API key configured', { hasApiKey: Boolean(config.apiKey) });

// NEVER this
logger.debug('Using API key', { key: config.apiKey }); // FORBIDDEN
logger.debug('Using API key', { key: config.apiKey.slice(0, 4) + '...' }); // STILL FORBIDDEN
```

---

## Input Validation

### Validation Strategy

```
External Input → Schema Validation → Type Coercion → Business Validation → Use
```

1. **Schema Validation**: Does it match expected shape? (Zod)
2. **Type Coercion**: Parse strings to proper types
3. **Business Validation**: Does it make sense? (e.g., is nodeId valid format?)
4. **Use**: Only now can you trust the data within this context

### Zod Schema Requirements

```typescript
// Define strict schemas for all external input
const SearchInputSchema = z.object({
  query: z.string().min(1).max(1000),
  limit: z.number().int().positive().max(100).default(20),
  includeArchived: z.boolean().default(false),
});

// Validate at the boundary
const validated = SearchInputSchema.parse(rawInput);
```

### Dangerous Input Patterns

Reject or sanitize:

- Path traversal attempts (`../`, `..\\`)
- Null bytes in strings
- Control characters in text input
- Excessively long strings
- Deeply nested structures

---

## Error Handling Security

### What Errors Should Reveal

To authenticated users:
- That an error occurred
- A correlation ID for support
- General category (network, validation, server error)
- Suggested remediation if applicable

To logs (internal):
- Full error stack traces
- Request context (without secrets)
- Timing information
- Related operation IDs

### What Errors Must Never Reveal

- Stack traces to end users
- Database structure or queries
- Internal service names or architecture
- File paths on the server
- Whether specific resources exist (for enumeration attacks)

```typescript
// User-facing error
throw new UserError('Unable to complete search. Please try again.', {
  correlationId: req.id,
  category: 'service_unavailable',
});

// Internal logging
logger.error('Workflowy API failed', {
  correlationId: req.id,
  statusCode: 503,
  endpoint: '/api/v2/search',
  duration: 2340,
  // Note: no request body, no auth headers
});
```

---

## Audit and Logging

### Events That Must Be Logged

| Event | Log Level | Required Fields |
|-------|-----------|-----------------|
| Server start/stop | INFO | version, config (redacted) |
| Destructive operations (delete, move) | INFO | operation, target IDs, user context |
| Authentication failures | WARN | attempt type, client info |
| Validation failures | WARN | field, violation type (not value) |
| Security-relevant errors | ERROR | correlation ID, error category |

### Log Sanitization

Before logging any object:

1. Remove all fields containing: password, key, token, secret, credential, auth
2. Truncate string fields over 500 characters
3. Redact personal identifiers if present
4. Never log request/response bodies containing user content

---

## Security Checklist for Code Review

Every PR should verify:

- [ ] No secrets in code, comments, or commit messages
- [ ] All external input validated with Zod schemas
- [ ] Error messages reveal no sensitive details
- [ ] Logging contains no secrets or user content
- [ ] Destructive operations are logged
- [ ] No new dependencies with known vulnerabilities
- [ ] Test coverage for security-relevant code paths

---

## Incident Response

If a security issue is discovered:

1. **Contain**: Disable affected functionality immediately
2. **Assess**: Determine scope and impact
3. **Notify**: Alert maintainers and affected users if data exposed
4. **Remediate**: Fix the vulnerability
5. **Review**: Update these principles if a gap exists

---

## See Also

- [Constitution](./constitution.md) - Core principles and mission
- [Architecture Principles](./principles-architecture.md) - Structural guidance
- [Development Principles](./principles-development.md) - Code-level guidance
- [Implementation Plan](./implementation-plan.md) - Technical approach
