
export default async function retok() {
  return {
    "tool.execute.after": async (input, output) => {
      if (input.tool !== "bash" || typeof output.output !== "string") return;
      const [text] = await compactTexts([output.output], input.tool);
      output.output = text;
    },
  };
}
