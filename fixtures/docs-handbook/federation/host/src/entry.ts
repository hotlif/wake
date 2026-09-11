const button = document.createElement("button");
button.textContent = "加载远程问候";
button.onclick = async () => {
  try {
    const { greeting } = await import("catalog/greeting");
    button.textContent = greeting("Wake");
  } catch (error) {
    button.textContent = "远程加载失败";
    console.error(error);
  }
};
document.body.append(button);
