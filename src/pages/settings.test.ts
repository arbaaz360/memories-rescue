import { describe, expect, it } from "vitest";
import { templateExample, templateProblem } from "./Settings";

describe("output template (mr-job tokens)", () => {
  it("accepts the default and shows an example name", () => {
    expect(templateProblem("{date}_{time}_{shortid}.{ext}")).toBeNull();
    expect(templateExample("{date}_{time}_{shortid}.{ext}")).toBe("2019-07-04_210355_3F2A9C1E.jpg");
  });

  it("names the problem", () => {
    expect(templateProblem("  ")).toBe("empty");
    expect(templateProblem("{YYYY-MM-DD}_{shortid}.{ext}")).toBe("token");
    expect(templateProblem("{date}/{shortid}.{ext}")).toBe("chars");
    expect(templateProblem("{date}_{time}.{ext}")).toBe("id");
    expect(templateProblem("{date}_{id}")).toBe("ext");
  });
});
