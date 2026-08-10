"""join_edge 候选的基数探测（只读）：两侧各 COUNT(*)/COUNT(DISTINCT col)。
card 规则：card = (左表侧重复度):(右表侧重复度)，1 = 该侧对连接键唯一。"""
import sys, json
sys.path.insert(0, r'D:\code\dms_ai\tools')
from settings import analysis_mysql_kwargs
import pymysql

CANDS = [
    ("t_after_sales_order_header", "sales_order_code", "t_sales_order", "sales_order_code"),
    ("t_after_sales_order_header", "owner_manager", "t_employee", "employee_id"),
    ("t_after_sales_order_detail", "after_sales_code", "t_after_sales_order_header", "after_sales_code"),
    ("t_customer", "area_manager_id", "t_employee", "employee_id"),
    ("t_customer_balance", "customer_code", "t_customer", "customer_code"),
    ("t_sales_order", "delivery_warehouse_code", "t_warehouse", "wms_code"),
    ("t_activity_freight_fee", "activity_id", "t_activity_main", "id"),
    ("t_activity_material_fee", "activity_id", "t_activity_main", "id"),
    ("t_activity_other_fee", "activity_id", "t_activity_main", "id"),
    ("t_activity_promoter_fee", "activity_id", "t_activity_main", "id"),
    ("t_activity_tasting_fee", "activity_id", "t_activity_main", "id"),
    ("t_activity_venue_fee", "activity_id", "t_activity_main", "id"),
    ("t_invoice_apply_detail", "invoice_code", "t_invoice_apply_header", "invoice_code"),
    ("t_sales_order_logistics", "sales_order_code", "t_sales_order", "sales_order_code"),
    ("t_customer_device_ledger", "sku_code", "t_goods", "goods_code"),
    ("t_account_bill_detail", "bill_code", "t_account_bill_header", "bill_code"),
]

c = pymysql.connect(**analysis_mysql_kwargs())
cur = c.cursor()
out = []
for lt, lc, rt, rc in CANDS:
    try:
        cur.execute(f"SELECT COUNT(*), COUNT(DISTINCT {lc}) FROM {lt}")
        ln, ld = cur.fetchone()
        cur.execute(f"SELECT COUNT(*), COUNT(DISTINCT {rc}) FROM {rt}")
        rn, rd = cur.fetchone()
        lcard = "1" if ln == ld else "N"
        rcard = "1" if rn == rd else "N"
        card = f"{lcard}:{rcard}"
        out.append({"lt": lt, "lc": lc, "rt": rt, "rc": rc, "card": card,
                    "left": [ln, ld], "right": [rn, rd]})
        print(f"{card}  {lt}.{lc} ({ld}/{ln}) → {rt}.{rc} ({rd}/{rn})")
    except Exception as e:
        print(f"ERR  {lt}.{lc} → {rt}.{rc}: {str(e)[:80]}")
json.dump(out, open(r'D:\code\dms_ai\_card.json', 'w', encoding='utf-8'), ensure_ascii=False, indent=1)
