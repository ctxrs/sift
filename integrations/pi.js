
export default function retok(pi) {
  pi.on("tool_result", async event => {
    if (!["bash", "powershell"].includes(event.toolName) || !Array.isArray(event.content)) return;
    const indices = [];
    const texts = [];
    event.content.forEach((block, index) => {
      if (block?.type === "text" && typeof block.text === "string") {
        indices.push(index);
        texts.push(block.text);
      }
    });
    const compacted = await compactTexts(texts, event.toolName);
    if (compacted.every((text, i) => text === texts[i])) return;
    const content = event.content.slice();
    indices.forEach((index, i) => { content[index] = {...content[index], text: compacted[i]}; });
    return {content};
  });
}
