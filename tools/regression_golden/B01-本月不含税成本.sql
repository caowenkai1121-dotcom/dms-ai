SELECT SUM(sf.cost_excluding_tax) AS `不含税成本` FROM sales_dw.dws_off_offline_sale_dfn sf WHERE sf.order_date >= DATE_FORMAT(CURDATE(),'%Y-%m-01') AND sf.order_date < DATE_ADD(CURDATE(), INTERVAL 1 DAY) LIMIT 200;

-- 明细
SELECT sf.order_date AS `日期`, sf.storecode AS `客户编码`, sf.storename AS `客户名称`, sf.skucode AS `商品编码`, sf.skuname AS `商品名称`, sf.war_zone AS `战区`, sf.region AS `省区`, sf.qty AS `数量`, sf.amount AS `销售额`, sf.cost_excluding_tax AS `不含税成本`, sf.revenue_excluding_tax AS `不含税收入`, sf.gross_profit AS `毛利额`, sf.gross_profit / NULLIF(sf.revenue_excluding_tax, 0) AS `毛利率` FROM sales_dw.dws_off_offline_sale_dfn sf WHERE sf.order_date >= DATE_FORMAT(CURDATE(),'%Y-%m-01') AND sf.order_date < DATE_ADD(CURDATE(), INTERVAL 1 DAY) ORDER BY sf.order_date DESC, ABS(sf.amount) DESC LIMIT 100
