
export default async function retok() {
  return {
    "tool.execute.after": async (input, output) => {
      if (!["bash", "shell"].includes(input.tool) || typeof output.output !== "string") return;
      const [text] = await compactTexts([output.output], input.tool);
      output.output = text;
    },
  };
}
