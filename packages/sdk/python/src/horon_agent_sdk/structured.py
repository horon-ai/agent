"""SDK-level structured output: JSON parsing plus JSON Schema validation.

The TypeScript SDK uses Ajv. Python's ``jsonschema`` reports the same keyword
names and instance paths, so validation behaviour matches; the human-readable
message is normalized to Ajv's phrasing where the wording differs, because
those messages are fed back to the model in a corrective prompt and the model
reads them.
"""

from __future__ import annotations

import json
import re
from typing import Any, TypedDict

import jsonschema

from .errors import HarnessError

StructuredOutputSchema = dict[str, Any]
"""JSON Schema accepted by :meth:`HoronAgentClient.run_structured`."""


class StructuredValidationIssue(TypedDict, total=False):
    """A normalized parse or JSON Schema validation problem."""

    # JSON Pointer path to the invalid value. "/" means the root.
    path: str
    # JSON Schema keyword, or "parse" when the text was not valid JSON.
    keyword: str
    # Human-readable validation message.
    message: str
    # Keyword-specific validation metadata, when available.
    params: dict[str, Any]


class StructuredOutputAttempt(TypedDict):
    """One model attempt made by ``run_structured``."""

    # One-based attempt number.
    attempt: int
    # Raw assistant text produced for this attempt.
    text: str
    # Empty for the successful attempt.
    errors: list[StructuredValidationIssue]


class StructuredOutputError(HarnessError):
    """Raised after all bounded structured-output attempts fail validation."""

    def __init__(self, attempts: list[StructuredOutputAttempt]) -> None:
        last = attempts[-1] if attempts else None
        count = len(attempts)
        summary = "; ".join(format_issue(issue) for issue in (last["errors"] if last else []))
        super().__init__(
            "structured_output_invalid",
            f"model did not produce valid structured output after {count} "
            f"attempt{'' if count == 1 else 's'}: {summary or 'no structured output attempts ran'}",
        )
        self.attempts = attempts
        self.validation_errors: list[StructuredValidationIssue] = list(last["errors"]) if last else []
        self.last_text = last["text"] if last else ""


class StructuredValidationResult:
    """Outcome of validating one assistant message against a schema."""

    __slots__ = ("data", "errors", "ok")

    def __init__(
        self,
        *,
        ok: bool,
        data: Any = None,
        errors: list[StructuredValidationIssue] | None = None,
    ) -> None:
        self.ok = ok
        self.data = data
        self.errors = errors or []


def _validator_for(schema: StructuredOutputSchema) -> Any:
    validator_cls = jsonschema.validators.validator_for(schema, default=jsonschema.Draft202012Validator)
    return validator_cls(schema)


def compile_structured_schema(schema: StructuredOutputSchema) -> Any:
    """Check a schema up front, so a typo fails before a model turn is spent."""
    try:
        validator_cls = jsonschema.validators.validator_for(schema, default=jsonschema.Draft202012Validator)
        validator_cls.check_schema(schema)
        return validator_cls(schema)
    except jsonschema.SchemaError as error:
        raise HarnessError("structured_schema_invalid", error.message) from error
    except Exception as error:
        raise HarnessError("structured_schema_invalid", str(error)) from error


def validate_structured_text(text: str, validator: Any) -> StructuredValidationResult:
    parsed = _parse_json_from_text(text)
    if parsed is None:
        return StructuredValidationResult(
            ok=False,
            errors=[{"path": "/", "keyword": "parse", "message": "invalid JSON: input was empty or unparseable"}],
        )
    value, parse_error = parsed
    if parse_error is not None:
        return StructuredValidationResult(
            ok=False,
            errors=[{"path": "/", "keyword": "parse", "message": f"invalid JSON: {parse_error}"}],
        )
    errors = list(validator.iter_errors(value))
    if not errors:
        return StructuredValidationResult(ok=True, data=value, errors=[])
    return StructuredValidationResult(ok=False, errors=[_normalize(error) for error in errors])


def build_structured_prompt(content: str, schema: StructuredOutputSchema) -> str:
    return f"{content}\n\n{_structured_instructions(schema)}"


def build_structured_correction_prompt(
    schema: StructuredOutputSchema,
    attempt: StructuredOutputAttempt,
) -> str:
    lines = [
        "Your previous response did not satisfy the required structured-output contract.",
        "Return a corrected response as JSON only, with no markdown, prose, code fences, or comments.",
        "It must validate against this JSON Schema:",
        "```json",
        stable_stringify(schema),
        "```",
        "Validation errors:",
        *(f"- {format_issue(issue)}" for issue in attempt["errors"]),
        "Previous response:",
        "```",
        truncate(attempt["text"], 4_000),
        "```",
    ]
    return "\n".join(lines)


def assert_retry_count(max_retries: int) -> None:
    if isinstance(max_retries, bool) or not isinstance(max_retries, int) or max_retries < 0:
        raise HarnessError("invalid_request", "maxRetries must be a non-negative safe integer")


def format_issue(issue: StructuredValidationIssue) -> str:
    path = issue.get("path") or "/"
    return f"{path} {issue.get('message', '')}"


def _structured_instructions(schema: StructuredOutputSchema) -> str:
    return "\n".join(
        [
            "Return the answer as JSON only, with no markdown, prose, code fences, or comments.",
            "The JSON value must validate against this JSON Schema:",
            "```json",
            stable_stringify(schema),
            "```",
        ]
    )


def _pointer(path: Any) -> str:
    parts = list(path)
    if not parts:
        return "/"
    escaped = [str(part).replace("~", "~0").replace("/", "~1") for part in parts]
    return "/" + "/".join(escaped)


def _normalize(error: jsonschema.ValidationError) -> StructuredValidationIssue:
    keyword = error.validator or "validation"
    message = _ajv_message(error)
    params: dict[str, Any] = {}
    if keyword == "required":
        params["missingProperty"] = (error.message or "").split("'")[1] if "'" in (error.message or "") else ""
    elif keyword == "additionalProperties":
        params["additionalProperty"] = (error.message or "").split("'")[1] if "'" in (error.message or "") else ""
    elif keyword in ("type", "minimum", "maximum", "minLength", "maxLength", "pattern", "enum", "const"):
        params["limit"] = error.validator_value
    return {
        "path": _pointer(error.absolute_path),
        "keyword": keyword,
        "message": message,
        "params": params,
    }


def _ajv_message(error: jsonschema.ValidationError) -> str:
    keyword = error.validator
    limit = error.validator_value
    if keyword == "type":
        if isinstance(limit, list):
            return "must be " + ",".join(str(item) for item in limit)
        return f"must be {limit}"
    if keyword == "required":
        match = re.search(r"'([^']+)'", error.message or "")
        return f"must have required property '{match.group(1)}'" if match else (error.message or "")
    if keyword == "additionalProperties":
        return "must NOT have additional properties"
    if keyword == "minimum":
        return f"must be >= {limit}"
    if keyword == "maximum":
        return f"must be <= {limit}"
    if keyword == "minLength":
        return f"must NOT have fewer than {limit} characters"
    if keyword == "maxLength":
        return f"must NOT have more than {limit} characters"
    if keyword == "enum":
        rendered = ", ".join(json.dumps(item, ensure_ascii=False) for item in limit)
        return f"must be equal to one of the allowed values: {rendered}"
    if keyword == "const":
        return f"must be equal to constant {json.dumps(limit, ensure_ascii=False)}"
    return error.message or f"failed {keyword} validation"


def _parse_json_from_text(text: str) -> tuple[Any, str | None] | None:
    candidates = _json_candidates(text)
    if not candidates:
        return None
    last_error = "input was empty"
    for candidate in candidates:
        try:
            return json.loads(candidate), None
        except json.JSONDecodeError as error:
            last_error = error.msg
    return None, last_error


def _json_candidates(text: str) -> list[str]:
    trimmed = text.strip()
    if not trimmed:
        return []

    candidates = [trimmed]
    fenced = re.match(r"^```(?:json)?\s*([\s\S]*?)\s*```$", trimmed, re.IGNORECASE)
    if fenced and fenced.group(1):
        candidates.append(fenced.group(1).strip())

    container = _first_json_container(trimmed)
    if container is not None and container not in candidates:
        candidates.append(container)
    return candidates


def _first_json_container(text: str) -> str | None:
    start = re.search(r"[\[{]", text)
    if start is None:
        return None
    start_index = start.start()

    stack: list[str] = []
    in_string = False
    escaped = False
    for index in range(start_index, len(text)):
        char = text[index]
        if in_string:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
            continue

        if char == '"':
            in_string = True
            continue
        if char in "{[":
            stack.append("}" if char == "{" else "]")
            continue
        if char in "}]":
            if not stack or stack.pop() != char:
                return None
            if not stack:
                return text[start_index : index + 1]
    return None


def stable_stringify(value: Any) -> str:
    return json.dumps(_sort_json(value), indent=2, ensure_ascii=False)


def _sort_json(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: _sort_json(value[key]) for key in sorted(value)}
    if isinstance(value, list):
        return [_sort_json(item) for item in value]
    return value


def truncate(value: str, max_chars: int) -> str:
    if len(value) <= max_chars:
        return value
    return f"{value[:max_chars]}\n… truncated {len(value) - max_chars} chars"
