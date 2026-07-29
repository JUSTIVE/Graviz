import { test, expect, type Page } from "@playwright/test";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
const here = dirname(fileURLToPath(import.meta.url));
// ~64 chars: wider than the 312px tooltip (80vw @390) so it marquees,
// but the node stays small enough that its rows are on-screen to hover.
const LONG = "f" + "ooValue".repeat(9);
function hashSdl(s0: string){const s=s0.trim();let h=5381;for(let i=0;i<s.length;i++)h=((h<<5)+h+s.charCodeAt(i))>>>0;return h.toString(36);}
interface Dim{id:string;rowH:number;headerH:number;w:number;h:number;cx:number;cy:number}
type D={__gqlCanvas?:{getLaidNodeCount():number;getLod():string;getView():{x:number;y:number;k:number};getNodeDimensions():Dim[];navigate(i:string):void}};
const sdl = `
type Query { root: Root }
type Root {
  """긴 단일 토큰 필드명 — marquee 되어야 함"""
  ${LONG}: String!
  """이름과 타입이 합쳐 폭을 넘으면 타입이 줄내림"""
  mediumFieldNameHere: SomeModeratelyLongReturnTypeName!
  short: Root
}
type SomeModeratelyLongReturnTypeName { id: ID }
`;
test.use({ viewport: { width: 390, height: 844 }, hasTouch: true });

async function boot(page: Page) {
  await page.addInitScript((e)=>localStorage.setItem("gompassql:history",JSON.stringify(e)),[{hash:hashSdl(sdl),sdl,name:"m.graphql",createdAt:Date.now(),updatedAt:Date.now()}]);
  await page.goto("/");
  await page.getByRole("button",{name:/Recent schemas/i}).click({timeout:10000});
  await page.getByRole("button",{name:/m\.graphql/i}).click({timeout:10000});
  await page.getByRole("button",{name:/^Visualize$/}).click({timeout:10000});
  await page.waitForURL(/\/view/,{timeout:30000});
  await page.evaluate(()=>(window as unknown as D).__gqlCanvas!.navigate("Root"));
  await page.waitForFunction(()=>(window as unknown as D).__gqlCanvas?.getLod()==="full",undefined,{timeout:30000});
  await page.waitForTimeout(1200);
  await page.getByText("View controls").first().click().catch(()=>{});
  await page.waitForTimeout(400);
}
async function hoverRow(page: Page, rowIdx: number) {
  const box=(await page.locator("canvas").first().boundingBox())!;
  const p=await page.evaluate((idx)=>{const a=(window as unknown as D).__gqlCanvas!;const v=a.getView();const n=a.getNodeDimensions().find(d=>d.id==="Root")!;const bodyTop=n.headerH+8-2;return{x:(n.cx-n.w/2+20)*v.k+v.x,y:(n.cy-n.h/2+bodyTop+(idx+0.5)*n.rowH)*v.k+v.y};},rowIdx);
  await page.mouse.move(p.x+box.x,p.y+box.y);
  await page.waitForTimeout(700);
}

test("long single token marquees inside the box", async ({ page }) => {
  test.setTimeout(120000);
  await boot(page);
  await hoverRow(page, 0);
  const r = await page.evaluate((long) => {
    const inner = Array.from(document.querySelectorAll("span")).find(
      (s) => s.textContent === long && s.className.includes("whitespace-nowrap"),
    );
    if (!inner) return { found: false };
    const box = inner.parentElement as HTMLElement;
    return { found: true, overflowing: inner.scrollWidth > box.clientWidth + 1, animating: inner.getAnimations().length > 0 };
  }, LONG);
  await page.screenshot({ path: join(here, "__screenshots__", "marquee-long.png") });
  expect(r.found).toBeTruthy();
  expect(r.overflowing).toBeTruthy();
  expect(r.animating).toBeTruthy();
});

test("name+type exceeding width wrap to two lines", async ({ page }) => {
  test.setTimeout(120000);
  await boot(page);
  await hoverRow(page, 1);
  const r = await page.evaluate(() => {
    const name = Array.from(document.querySelectorAll("span")).find((s) => s.textContent === "mediumFieldNameHere");
    const type = Array.from(document.querySelectorAll("span")).find((s) => s.textContent === "SomeModeratelyLongReturnTypeName!");
    if (!name || !type) return { found: false };
    const nb = (name.parentElement as HTMLElement).getBoundingClientRect();
    const tb = (type.parentElement as HTMLElement).getBoundingClientRect();
    return { found: true, wrapped: tb.top >= nb.bottom - 2 };
  });
  await page.screenshot({ path: join(here, "__screenshots__", "marquee-wrap.png") });
  expect(r.found).toBeTruthy();
  expect(r.wrapped).toBeTruthy();
});
