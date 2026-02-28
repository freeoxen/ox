import { describe, expect, test } from "bun:test";
import { parseParamSignature, compileTool } from "./tool-compiler";

describe("parseParamSignature", () => {
  test("parses empty signature", () => {
    const schema = parseParamSignature("()");
    expect(schema).toEqual({
      type: "object",
      properties: {},
      required: [],
    });
  });

  test("parses bare empty string", () => {
    const schema = parseParamSignature("");
    expect(schema).toEqual({
      type: "object",
      properties: {},
      required: [],
    });
  });

  test("parses single required param", () => {
    const schema = parseParamSignature("(text: string)");
    expect(schema).toEqual({
      type: "object",
      properties: { text: { type: "string" } },
      required: ["text"],
    });
  });

  test("parses single optional param", () => {
    const schema = parseParamSignature("(count?: number)");
    expect(schema).toEqual({
      type: "object",
      properties: { count: { type: "number" } },
      required: [],
    });
  });

  test("parses multiple params with mixed optionality", () => {
    const schema = parseParamSignature(
      "(text: string, count?: number, flag: boolean)",
    );
    expect(schema).toEqual({
      type: "object",
      properties: {
        text: { type: "string" },
        count: { type: "number" },
        flag: { type: "boolean" },
      },
      required: ["text", "flag"],
    });
  });

  test("handles integer type", () => {
    const schema = parseParamSignature("(n: integer)");
    expect(schema).toEqual({
      type: "object",
      properties: { n: { type: "integer" } },
      required: ["n"],
    });
  });

  test("is case-insensitive for types", () => {
    const schema = parseParamSignature("(x: String, y: NUMBER)");
    expect(schema.properties.x).toEqual({ type: "string" });
    expect(schema.properties.y).toEqual({ type: "number" });
  });

  test("strips surrounding whitespace", () => {
    const schema = parseParamSignature("  ( text : string )  ");
    expect(schema.properties.text).toEqual({ type: "string" });
    expect(schema.required).toEqual(["text"]);
  });

  test("works without parens", () => {
    const schema = parseParamSignature("text: string");
    expect(schema.properties.text).toEqual({ type: "string" });
  });

  test("throws on unparseable param", () => {
    expect(() => parseParamSignature("(bad param)")).toThrow("cannot parse");
  });

  test("throws on unknown type", () => {
    expect(() => parseParamSignature("(x: object)")).toThrow("unknown type");
  });

  test("skips empty segments from trailing comma", () => {
    const schema = parseParamSignature("(a: string,)");
    expect(Object.keys(schema.properties)).toEqual(["a"]);
  });
});

describe("compileTool", () => {
  test("compiles and executes a simple tool", () => {
    const { schemaJson, callback } = compileTool({
      name: "add",
      description: "adds two numbers",
      params: "(a: number, b: number)",
      body: "return a + b;",
    });

    const schema = JSON.parse(schemaJson);
    expect(schema.properties.a).toEqual({ type: "number" });
    expect(schema.properties.b).toEqual({ type: "number" });
    expect(schema.required).toEqual(["a", "b"]);

    expect(callback('{"a": 1, "b": 2}')).toBe("3");
  });

  test("compiles reverse_text builtin", () => {
    const { callback } = compileTool({
      name: "reverse_text",
      description: "Reverse the characters in a string",
      params: "(text: string)",
      body: 'return text.split("").reverse().join("");',
    });

    expect(callback('{"text": "hello"}')).toBe("olleh");
  });

  test("returns error string on runtime failure", () => {
    const { callback } = compileTool({
      name: "bad",
      description: "throws",
      params: "(x: string)",
      body: 'throw new Error("boom");',
    });

    expect(callback('{"x": "a"}')).toBe("error: boom");
  });

  test("returns error string for non-Error throw", () => {
    const { callback } = compileTool({
      name: "bad",
      description: "throws string",
      params: "(x: string)",
      body: 'throw "oops";',
    });

    expect(callback('{"x": "a"}')).toBe("error: oops");
  });

  test("compiles tool with no params", () => {
    const { schemaJson, callback } = compileTool({
      name: "greet",
      description: "says hi",
      params: "()",
      body: 'return "hi";',
    });

    const schema = JSON.parse(schemaJson);
    expect(schema.properties).toEqual({});
    expect(callback("{}")).toBe("hi");
  });
});
